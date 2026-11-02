-- magpie's schema (magpie spec 4), in one migration. Shipped migrations are never edited; later ones are expand-only
-- within a release (old and new replicas run together during a rollout, magpie 18.3).
--
-- The importer's tables (cutover spec 7.3) are here too; they are dropped by a later migration after the switch.
-- The switch window's own tables and columns (tickets, the fence, import_uploads, uploads.legacy_path) are WP-51's
-- migration.

create extension if not exists pg_trgm;

-- A person known to this deployment (created on first request).
create table persons (
  person                text primary key check (person = lower(btrim(person)) and position('@' in person) > 1),
  person_key            text not null unique,           -- 'u' || left(sha256hex(person), 24)
  created_at            timestamptz not null default now(),
  change_generation     uuid not null default gen_random_uuid(),  -- the change feed's generation (never changes)
  change_seq            bigint not null default 0,      -- head of this person's change feed
  purged_conversations  bigint not null default 0       -- counter for the ops 'purged' figure
);

create table conversations (
  id                    uuid primary key,
  person                text not null references persons(person),
  create_key            text,                           -- pigeon's idempotencyKey for the create
  title                 text,                           -- null = untitled (pigeon shows its placeholder)
  name_source           text check (name_source in ('title','user')),  -- null = provisional
  title_attempts        smallint not null default 0,
  created_at            timestamptz not null,
  activity_at           timestamptz not null,           -- list order (updatedAt): create, turn start, turn end
  last_user_message_at  timestamptz,
  deleted_at            timestamptz,                    -- set = in Recently deleted
  head_seq              bigint not null default 0,      -- last event seq (contiguous from 1)
  row_head              bigint not null default 0,      -- last timeline row seq (contiguous from 1)
  epoch                 bigint not null default 1,      -- timeline epoch; +1 when visible history is rewritten
  cmd_head              bigint not null default 0,      -- last command seq
  run_state             text not null default 'idle'
                        check (run_state in ('idle','admitted','preparing','running','stopping','held')),
  active_turn           uuid,
  last_turn             uuid,                           -- the most recent ended turn (Conversation.lastTurn)
  background            jsonb not null default '[]',   -- background tasks the supervisor reports
  sandbox_phase         text check (sandbox_phase in ('preparing','ready')),  -- Conversation.sandbox
  provider_session_id   text,                           -- Claude Code's session id
  main_transcript       text,                           -- 'projects/-workspace/<provider_session_id>.jsonl' (a path)
  model                 text,                           -- model of the latest assistant line
  context_tokens        integer,                        -- input+cache tokens of the latest assistant line
  cc_bytes              bigint not null default 0,      -- storage accounting for the quota
  cc_resync             boolean not null default false, -- set by cc.write_stuck (section 6.12)
  lease_id              uuid,
  lease_sandbox         uuid,
  lease_epoch           bigint not null default 0,
  heartbeat_at          timestamptz,
  hermit_sandbox        uuid,                           -- the live sandbox per hermit's feed (section 8.7)
  hermit_state          text,                           -- its state: starting|ready|paused|releasing|gone
  hermit_version        bigint,
  migrating             boolean not null default false, -- the importer's lock (cutover spec 6)
  origin                jsonb not null default '{"kind":"new"}',
  unique (person, create_key)
);
create index conversations_live    on conversations (person, activity_at desc, id) where deleted_at is null;
create index conversations_deleted on conversations (person, deleted_at desc, id) where deleted_at is not null;
create index conversations_purge   on conversations (deleted_at) where deleted_at is not null;
create index conversations_title   on conversations using gin (title gin_trgm_ops);
create index conversations_sandbox on conversations (hermit_sandbox) where hermit_sandbox is not null;

-- The per-person change feed (M12). One row per change; dense seq per person.
create table changes (
  person          text not null references persons on delete cascade,
  seq             bigint not null,
  at              timestamptz not null default now(),
  type            text not null check (type in ('conversation_upserted','conversation_removed','turn','sandbox',
                                                'ask','ask_resolved','timeline_replaced')),
  conversation_id uuid not null,
  data            jsonb not null,        -- the type-specific fields (section 7.6); upserted reads the live row
  primary key (person, seq)
);

-- The event log. Rows are immutable once written.
create table events (
  conversation_id uuid not null references conversations on delete cascade,
  seq             bigint not null,
  kind            text not null,
  at              timestamptz not null,
  turn_id         uuid,
  data            jsonb not null,
  meta            jsonb,                 -- computed at ingest, never changed afterwards
  text            text,                  -- plain text for search and previews, <= 64 KiB
  is_prompt       boolean not null default false,
  viewer          text,                  -- admin email when sent while viewing as the person
  primary key (conversation_id, seq)
);
create index events_prompts on events (conversation_id, seq) where is_prompt;
create index events_text    on events using gin (text gin_trgm_ops);

-- The timeline projection (section 7.3): protocol-neutral rows, written in the same transaction as the events
-- they come from, under the conversation row lock.
create table rows (
  conversation_id uuid not null references conversations on delete cascade,
  row_id          bigint not null,       -- the row seq of its first version (Row.seqStart); stable identity
  seq_end         bigint not null,       -- the row seq of its current version
  kind            text not null check (kind in ('user','assistant','reasoning','tool','error')),
  at              timestamptz not null,
  turn_id         uuid,
  item            jsonb not null,        -- RowItem (section 7.3)
  source_seqs     bigint[] not null,     -- the event seqs this row was built from
  tool_use_id     text,                  -- tool rows
  message_id      text,                  -- assistant/reasoning/tool rows: the API message id
  block_index     integer,               -- and the content block index within that message
  is_prompt       boolean not null default false,
  primary key (conversation_id, row_id)
);
create unique index rows_end  on rows (conversation_id, seq_end);
create index rows_tool        on rows (conversation_id, tool_use_id) where tool_use_id is not null;
create index rows_prompts     on rows (conversation_id, row_id) where is_prompt;

-- Every row seq ever issued, and the row it belongs to (makes `after` paging dense, section 7.4).
create table row_seqs (
  conversation_id uuid not null references conversations on delete cascade,
  seq             bigint not null,
  row_id          bigint not null,
  primary key (conversation_id, seq)
);

-- Live (partial) assistant output of running turns (section 7.5). Ephemeral: unlogged, never backed up.
create unlogged table live_frames (
  conversation_id uuid not null,
  turn_id         uuid not null,
  n               bigint not null,       -- per turn, contiguous from 1 (the supervisor's counter)
  message_id      text,
  block_index     integer,
  op              text not null check (op in ('start','delta','stop','status','gap')),
  block           text,                  -- start: text|thinking|tool_use
  name            text,                  -- start of a tool_use block
  byte_offset     bigint,                -- delta: offset of `text` within the block
  text            text,
  status          jsonb,                 -- status frames (api_retry …)
  at              timestamptz not null default now(),
  primary key (turn_id, n)
);
create index live_frames_conv on live_frames (conversation_id, turn_id, n);

-- tool_use id -> the events that carry the call and its result.
create table tool_links (
  conversation_id uuid not null references conversations on delete cascade,
  tool_use_id     text not null,
  use_seq         bigint not null,
  result_seq      bigint,
  name            text not null,
  primary key (conversation_id, tool_use_id)
);

create table turns (
  id                uuid primary key,
  conversation_id   uuid not null references conversations on delete cascade,
  origin            text not null check (origin in ('person','agent','import')),
  state             text not null check (state in
                    ('admitted','preparing','starting','running','stopping','completed','failed','canceled')),
  client_message_id text,
  user_seq          bigint,        -- the user.sent event
  start_seq         bigint,        -- the turn.started event
  end_seq           bigint,        -- the turn.ended event
  start_cmd_seq     bigint,        -- the start_turn command
  echo_matched      boolean not null default false,
  created_at        timestamptz not null default now(),
  ready_at          timestamptz,   -- hermit said ready
  started_at        timestamptz,   -- the supervisor said running
  ended_at          timestamptz,
  stop_requested_at timestamptz,
  stop_by           text,
  error_code        text,
  error_detail      text,
  usage             jsonb,         -- informational, from the supervisor's end report
  duration_ms       bigint,
  sandbox_id        uuid,
  driver            text,          -- replica that drives acquisition (pod name)
  driver_until      timestamptz,   -- claim expiry; another replica takes over after it
  deadline          timestamptz,   -- the current state's timeout (section 8.8 defines it per state)
  unique (conversation_id, client_message_id)
);
create index turns_open on turns (deadline) where state not in ('completed','failed','canceled');

-- Work for the supervisor (long-polled, section 8.4).
create table commands (
  conversation_id uuid not null references conversations on delete cascade,
  cmd_seq         bigint not null,
  type            text not null check (type in ('start_turn','interrupt','kill','title','answer_ask','stop_tasks')),
  turn_id         uuid,
  payload         jsonb not null,
  created_at      timestamptz not null default now(),
  delivered_at    timestamptz,
  acked_at        timestamptz,
  result          jsonb,
  canceled_at     timestamptz,
  primary key (conversation_id, cmd_seq)
);

-- Idempotency of supervisor reports that carry an id (task events), kept 24 h.
create table report_ids (
  conversation_id uuid not null references conversations on delete cascade,
  id              text not null,
  at              timestamptz not null default now(),
  primary key (conversation_id, id)
);

-- Claude Code state files. scope = a conversation id, or a share's frozen snapshot id.
create table cc_files (
  id        bigserial primary key,
  scope     uuid not null,
  path      text not null,                  -- relative to ~/.claude, e.g. projects/-workspace/<sid>.jsonl
  kind      text not null check (kind in ('dir','lines','bytes')),
  mode      integer not null,
  mtime_ns  bigint not null,
  size      bigint not null default 0,
  version   bigint not null default 0,
  tail      bytea not null default '',      -- bytes after the last '\n' (lines files only)
  next_ord  bigint not null default 0,
  last_op_at timestamptz not null default now(),  -- for the limbo debounce (6.7)
  unique (scope, path)
);
create table cc_lines (
  file_id   bigint not null references cc_files on delete cascade,
  ord       bigint not null,                -- order in the file; never reused
  start_off bigint not null,
  len       integer not null,               -- bytes including the final '\n'
  sha256    bytea not null,                 -- of the complete reconstructed line
  stub      bytea not null,                 -- the line with each extracted string's content removed
  spans     jsonb,                          -- [[stub_offset, "<sha256 hex>"], …] ascending
  event_seq bigint,                         -- the event this line became (main transcript, visible kinds)
  turn_id   uuid,
  primary key (file_id, ord)
);
create index cc_lines_off on cc_lines (file_id, start_off);
create table cc_limbo (                     -- lines awaiting reuse: removed by a truncate, or moved off the main path
  scope      uuid not null,
  file_id    bigint,                        -- truncate: the file; moved: null (matched by the main path)
  reason     text not null check (reason in ('truncated','moved')),
  sha256     bytea not null,
  event_seq  bigint not null,
  removed_at timestamptz not null default now(),
  primary key (scope, reason, event_seq)
);
create table cc_pages (                     -- 'bytes' files, 256 KiB pages
  file_id bigint not null references cc_files on delete cascade,
  page_no integer not null,
  bytes   bytea not null,
  primary key (file_id, page_no)
);
create table cc_ops (                       -- idempotency of FUSE-backing operations, kept 24 h
  scope  uuid not null,
  op_id  uuid not null,
  result jsonb not null,
  at     timestamptz not null default now(),
  primary key (scope, op_id)
);

create table blobs (
  sha256      bytea primary key,             -- of the stored bytes
  size        bigint not null,
  kind        text not null check (kind in ('media','text','file')),
  encoding    text not null check (encoding in ('base64-json','json-string','raw')),
  mime        text,
  width       integer,
  height      integer,
  stored      boolean not null default true, -- false while the bytes are only in blob_staging (5.2)
  created_at  timestamptz not null default now(),
  last_ref_at timestamptz not null default now()
);
-- Bytes of blobs introduced by a cc write, committed with the write and moved to the bucket afterwards (5.2).
create table blob_staging (
  sha256      bytea primary key references blobs,
  bytes       bytea not null,
  attempts    integer not null default 0,
  next_at     timestamptz not null default now(),
  created_at  timestamptz not null default now()
);
create table blob_refs (
  owner      uuid not null,                  -- conversation id, share snapshot id or upload id
  owner_kind text not null check (owner_kind in ('conversation','share','upload')),
  blob       bytea not null references blobs,
  primary key (owner, blob)
);
create index blob_refs_blob on blob_refs (blob);

create table uploads (
  id              uuid primary key,
  person          text not null references persons,
  conversation_id uuid references conversations on delete cascade,  -- set when attached to a turn
  role            text not null check (role in ('file','image')),
  name            text not null,
  mime            text not null,
  size            bigint not null,
  blob            bytea not null references blobs,
  model_blob      bytea references blobs,     -- images: the copy sent to the model (also the 'display' variant)
  width           integer,
  height          integer,
  workspace_path  text,                       -- 'uploads/<name>' once attached
  created_at      timestamptz not null default now(),
  attached_seq    bigint
);
create unique index uploads_path on uploads (conversation_id, workspace_path) where workspace_path is not null;

create table sent_staging (                   -- phase 1 of a sent-file snapshot (section 11.2)
  conversation_id uuid not null references conversations on delete cascade,
  tool_use_id     text not null,
  files           jsonb not null,
  caption         text,
  created_at      timestamptz not null default now(),
  primary key (conversation_id, tool_use_id)
);
create table sent_files (
  conversation_id uuid not null references conversations on delete cascade,
  tool_use_id     text not null,
  path            text not null,              -- relative to /workspace
  name            text not null,
  size            bigint not null,
  mime            text,
  is_image        boolean not null,
  blob            bytea not null references blobs,
  caption         text,
  sent_at         timestamptz not null,
  seq             bigint not null,            -- the files.sent event
  primary key (conversation_id, tool_use_id, path)
);

create table shares (
  id                  text primary key check (id ~ '^[A-Za-z0-9_-]{22}$'),
  conversation_id     uuid,                    -- null once the source is purged
  owner               text not null,
  owner_name          text not null,           -- local part of the owner's email
  title               text,
  upto_seq            bigint not null,         -- last event seq included
  snapshot            uuid not null,           -- cc scope and blob owner of the frozen Claude Code state
  provider_session_id text,
  workspace_id        uuid not null,           -- uuidv5(SHARE_NS, 'share:' || id): hermit workspace (12.1)
  created_at          timestamptz not null default now(),
  revoked_at          timestamptz,
  revoke_reason       text check (revoke_reason in ('owner','deleted','offboarded'))
);
create index shares_owner on shares (owner, conversation_id, created_at desc) where revoked_at is null;
-- The frozen timeline of a share (rows as they were at the freeze; section 12.1).
create table share_rows (
  share_id   text not null references shares on delete cascade,
  row_id     bigint not null,
  seq_end    bigint not null,
  kind       text not null,
  at         timestamptz not null,
  turn_id    uuid,
  item       jsonb not null,
  primary key (share_id, row_id)
);

create table view_as_log (
  id              bigserial primary key,
  at              timestamptz not null default now(),
  admin           text not null,
  person          text not null,
  session         text not null,
  event           text not null check (event in ('start','create','send','cancel','upload','admin_read')),
  conversation_id uuid,
  seq             bigint,
  chars           integer
);

create table outbox (                          -- calls to hermit that must happen exactly once eventually
  id         bigserial primary key,
  kind       text not null check (kind in ('release_sandbox','delete_workspace')),
  payload    jsonb not null,
  attempts   integer not null default 0,
  next_at    timestamptz not null default now(),
  last_error text,
  created_at timestamptz not null default now()
);

-- hermit's sandbox-events feed cursor (section 8.7), written by the job leader only.
create table hermit_feed (
  id         boolean primary key default true check (id),
  after_seq  bigint,                           -- null = start from 'latest'
  updated_at timestamptz not null default now()
);

-- Whether magpie as a whole was able to accept supervisor calls (section 8.8).
create table availability (
  id               boolean primary key default true check (id),
  available_since  timestamptz not null default now(),
  last_tick        timestamptz not null default now()
);

-- Postgres TOAST compression for the large columns (magpie 4).
alter table cc_lines     alter column stub  set compression lz4;
alter table events       alter column data  set compression lz4;
alter table rows         alter column item  set compression lz4;
alter table blob_staging alter column bytes set compression lz4;
alter table cc_pages     alter column bytes set compression lz4;

-- The open ask of a conversation (Conversation.asks, magpie 9.1) is read from its ask events.
create index events_asks on events (conversation_id, seq) where kind in ('ask.pending', 'ask.resolved');

-- The importer's own tables (cutover spec 7.3), dropped after the switch.
-- One row per imported conversation. Native conversations have none.
create table import_sources (
  conversation_id   uuid primary key references conversations on delete cascade,
  person            text not null,
  user_key          text not null,           -- 'u' || left(sha256hex(person), 24), checked against the schema name
  source_schema     text not null,           -- cc_u...
  provider_session  text,                    -- Claude Code session id (the transcript's file name)
  row_fp            text not null,           -- sha256 of the imported sessions-row columns (7.4.1), canonical JSON
  files             jsonb not null,          -- {"<cc path>": {"src": "<path under /jfs>", "size": int, "mtime_ns": int,
                                             --   "ino": int, "sha256": "<hex of the whole source file at last sync>"}}
  sent_fp           text not null,           -- sha256 of the sorted (path, size, mtime_ns) list under sent/<conv>/
  uploads_done      text[] not null default '{}',   -- upload file names already attached
  failed_turns_fp   text not null default '',
  first_imported_at timestamptz not null,
  last_synced_at    timestamptz not null,
  last_change_at    timestamptz not null,    -- last pass that saw a source change
  verified_at       timestamptz,             -- pre-verification (switch-final A §3.3); the release only re-stats
  verified_stats    jsonb,                   -- (size, mtime_ns, ino) of every source + every fingerprint at verify
  released_at       timestamptz,             -- set null again by a reverse hand-over (14.2)
  release_reason    text check (release_reason in ('idle', 'forced')),
  last_error        text
);
create index import_sources_open on import_sources (person) where released_at is null;

create table import_shares (
  share_id     text primary key,               -- cc_shared.shares.id (22 chars)
  src_fp       text not null,                  -- sha256 of the row + the share folder listing
  imported_at  timestamptz not null
);

create table import_control (
  id                   boolean primary key default true check (id),
  handover_allowed_at  timestamptz,            -- the gate (6.3)
  hold                 boolean not null default false,  -- forces phase sync (no release)
  connectors_off_at    timestamptz,            -- the runner recorded the old connectors at 0 (the gate's input)
  all_owned_at         timestamptz,            -- every import_sources row released
  export_epoch         bigint not null default 0        -- 14.3
);
insert into import_control default values;
