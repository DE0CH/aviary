# magpie: implementation spec

Status: design for implementers, 2026-11-02, revision 2 (review round 1 applied; section 23 lists every review point
and where it landed, section 24 the points not taken). Authoritative inputs: `magpie-target-design.md` (Deyao,
2026-11-02) and the decisions in the refactor brief. Sources of today's behaviour: `inventory.json` (296 features
that name magpie) and gap reports 0-3. Every inventory feature whose target names magpie is listed in the coverage
table (section 22). Platform conventions (`spec-platform.md` section 3: ports, `Aviary-*` headers, camelCase JSON,
lower_snake_case error codes, pair tokens, readiness, drain) are binding and are written out in this spec, so nothing
here contradicts the platform table R1-R25; section 21.3 lists the rows the platform table must add (R26-R31).

magpie owns conversations. It is the only store of: the conversation list, titles, deletions, shares, the event log
(seq per conversation), the timeline rows projected from it (row seq per conversation), Claude Code's
per-conversation state files (served to the supervisor's FUSE layer), uploads, images and large tool output (blobs
in the bucket), the files panel and sent-file snapshots, the per-person change feed and the view-as audit. It is
written in Rust, lives in the public repo DE0CH/aviary, talks only REST, and owns its Postgres database and its bucket
behind its API. The same build runs in every deployment; nothing in it knows about YYC, JuiceFS, AWS, OpenRouter or
Cloudflare.

Contents
1. Boundaries and callers
2. Crate layout
3. Authentication and identity
4. Postgres schema
5. Blobs and the bucket
6. Claude Code state behind the FUSE layer
7. Events, timeline rows, live frames, the change feed
8. Turns: admission, the driver, commands, reports, cancel, failure
9. Conversations: create, list, titles, rename, delete, restore, purge
10. Uploads and attachments
11. Files panel and sent-file snapshots
12. Shares
13. View-as, audit and admin reads
14. Live fan-out across replicas
15. Background jobs
16. REST reference (every route)
17. Limits
18. Deployment and operations
19. The one-off importer
20. Tests and acceptance
21. Decisions taken in this spec, and what the other specs must provide
22. Coverage table
23. Review round 1: where each point landed
24. Rejected review points

---

## 1. Boundaries and callers

magpie is called by four callers, each authenticated by its own pair token (section 3):

| Caller | Reaches magpie how | Route prefixes it may use |
|---|---|---|
| pigeon | Service `magpie` (`http://magpie`, port 80 → 8080) | `/v1/conversations/**`, `/v1/creations/**`, `/v1/changes`, `/v1/uploads`, `/v1/blobs/**`, `/v1/shares/**`, `/v1/view-as` |
| gateway (YYC: egress), on behalf of the supervisor | inside the sandbox the supervisor calls the fixed endpoint name `magpie` (`http://magpie/…`); the gateway verifies the sandbox and forwards to `http://magpie` with its own token and the sandbox's identity headers | `/v1/sandbox/**` |
| hermit | Service `magpie` | `/v1/hermit/**`, `/v1/ops/**` |
| admin (operator tooling: Claude, runbooks, acceptance scripts) | Service `magpie` | `/v1/ops/**`, `/v1/admin/**` |

magpie calls exactly one service: hermit, at the fixed endpoint name `hermit` (`http://hermit`). It never calls the
supervisor (the supervisor long-polls magpie for commands, section 8.4; magpie does not use hermit's control token or
the supervisor's :7070), never calls pigeon (pigeon long-polls magpie), never calls egress or the model.

What magpie does NOT own, and who does:
- Sign-in, the Paseo protocol, the pure mapping of magpie's protocol-neutral timeline rows to Paseo rows (one row in,
  one Paseo row out, no state), signed blob URLs, localised strings: pigeon.
- Who may use the deployment, roles (admin/staff), sandbox lifecycle, `/workspace` storage (YYC: JuiceFS, its format,
  backups, mounts and clones), warm hints, per-sandbox secrets, the title system prompt and the managed CLAUDE.md
  text: hermit.
- Running Claude Code, its argv, the FUSE mount, the non-conversation parts of `~/.claude`, the SendUserFile hook,
  writing uploads into `/workspace/uploads`: supervisor.
- Model access, model list, cost, spend, budget: egress (the gateway's `model` endpoint). magpie has no cost or
  budget data; it only records what Claude Code wrote (for example `API Error: 402 …`) and tags it with an error code.

No deployment switches. magpie's environment carries only bindings (where its database and bucket are, its own
credentials, its pod name) and never a flag that changes behaviour. Every limit and timeout is a constant in
`limits.rs` (section 17).

---

## 2. Crate layout

DE0CH/aviary is one Cargo workspace (platform section 4). magpie's part:

```
aviary/
  crates/
    magpie-api/                   # serde types (rename_all = "camelCase") for every magpie request, response,
                                  # event kind, row item and change, plus a small reqwest client. The one contract
                                  # crate for magpie: used by pigeon, the supervisor, hermit and the ops tools.
      src/lib.rs
      src/conversations.rs  src/events.rs  src/rows.rs  src/live.rs  src/changes.rs  src/turns.rs
      src/sandbox.rs  src/shares.rs  src/files.rs  src/uploads.rs  src/ops.rs  src/errors.rs  src/client.rs
    magpie/                       # library + binary `magpie` (`magpie serve`, `magpie import …`)
      src/main.rs                 # tokio runtime, config from env, signal handling, drain (aviary-core)
      src/app.rs                  # axum Router assembly, shared state
      src/limits.rs               # every numeric limit and timeout (section 17), nothing else configurable
      src/auth.rs                 # route-to-caller table, pair tokens, identity and view-as headers
      src/http/                   # one module per route group (handlers only, no SQL)
        conversations.rs creations.rs timeline.rs changes.rs turns.rs uploads.rs files.rs shares.rs blobs.rs
        viewas.rs sandbox_cc.rs sandbox_turns.rs sandbox_live.rs sandbox_files.rs hermit.rs ops.rs admin.rs
      src/db/                     # aviary-pg pool, migrations runner, row types
        migrations/0001_init.sql …
      src/events/                 # append (seq allocation), kinds, tool links
      src/rows/                   # the timeline projection (section 7.3), row seq, epochs, paging
      src/live.rs                 # live frames (section 7.5)
      src/changes.rs              # the person change feed (section 7.6)
      src/fanout.rs               # LISTEN/NOTIFY hub (section 14)
      src/ccfs/                   # Claude Code state model (section 6)
        scan.rs extract.rs classify.rs lines.rs limbo.rs pages.rs reconstruct.rs lease.rs
      src/blobs/                  # staging + uploader, S3 store, LRU cache, thumbnails, display copies, model copies
      src/turns/                  # admit, driver, commands, supervisor reports, heartbeat reconcile, sweeper, codes
      src/hermit/                 # hermit client and the sandbox-events follower (section 8.7)
      src/conversations/          # create, list, rename, titles, delete/restore/purge
      src/shares.rs  src/viewas.rs
      src/jobs/                   # leader lock, purge, blob GC, outbox, retention, availability tick
      tests/                      # integration tests against a real Postgres and an S3 stub
    magpie-import/                # library linked into `magpie import …` (R15); deleted after the switch
```

Libraries: tokio, axum 0.8 (HTTP/1.1 and h2c), `aviary-pg` (tokio-postgres + deadpool-postgres, R9), aws-sdk-s3
(path-style, any S3-compatible endpoint), image + fast_image_resize + libheif-rs + webp (thumbnails, display and model
copies), kamadak-exif (orientation), sha2, base64, serde/serde_json, uuid (v4, v5, v7), time, tracing +
tracing-subscriber (JSON), metrics-exporter-prometheus, `aviary-core` (auth, drain, health, version).

---

## 3. Authentication and identity

### 3.1 Callers

Every request carries `Authorization: Bearer <pair token>` and `Aviary-Request: <uuid v7>` (logged as `request`,
forwarded unchanged to hermit), plus `Aviary-Protocol: magpie-person=<n>` or `magpie-sandbox=<n>` (platform 3.4).
magpie reads one env per caller (platform 3.7): `MAGPIE_ACCEPT_PIGEON`, `MAGPIE_ACCEPT_GATEWAY`,
`MAGPIE_ACCEPT_HERMIT`, `MAGPIE_ACCEPT_ADMIN`, each a comma-separated list (current first; two during a rotation).
Comparison is constant-time on SHA-256 digests (`aviary-core::auth`). A missing or unknown token is 401
`unauthenticated`; a valid token on a route its caller may not use is 403 `wrong_caller` (table in section 1). A
missing or empty `MAGPIE_ACCEPT_*` is a start failure. Outbound, magpie presents `MAGPIE_TOKEN_HERMIT` to hermit.
These replace today's shared `JARVIS_CC_TOKEN` and `JARVIS_SENT_FILES_TOKEN`, which disappear.

### 3.2 The person (pigeon routes)

pigeon signs the person in (asking hermit who may) and sends:
- `Aviary-Person: <email>`: required on every pigeon route except `GET /v1/blobs/{blobId}` (a capability pigeon
  signed, pigeon 5.9) and `POST /v1/view-as` (its body names the admin and the person). On share reads it names the viewer. magpie normalises it (`lower(btrim())`) and creates the
  `persons` row on first sight.
- `Aviary-Viewer: <admin email>` and `Aviary-View-Session: <≤ 64 chars>`: present only while an admin views as the
  person. magpie trusts pigeon that the viewer is an admin (pigeon checked hermit's role).

Authorisation is ownership: a conversation is visible only to its `person`. Anything else answers 404 `not_found`
(never 403, so ids don't leak). magpie keeps `person_key = 'u' + first 24 hex of sha256(person)` on the person row,
bit-identical to today's `userKey` (platform 3.8), for the importer and for log fields (logs never carry the email).

### 3.3 The sandbox (gateway routes)

The gateway verifies the sandbox's own token with hermit (`GET /v1/sandbox-tokens/{sha256}`), strips any
client-supplied `Aviary-*` headers, and adds:
- `Aviary-Sandbox-Id: <hermit's sandbox id>`
- `Aviary-Sandbox-Conversation: <conversation uuid>`
- `Aviary-Sandbox-Person: <email>`

magpie checks that the conversation exists and belongs to that person. The supervisor additionally carries
`Aviary-Lease: <lease uuid>` on every state-changing `/v1/sandbox/**` call and on the command poll (section 6.9).

### 3.4 hermit and admin

hermit and admin routes carry no person header; their bodies or paths name what they act on. Admin reads that show
conversation content (`/v1/admin/conversations/**`) require `Aviary-Admin: <email>` and are audited (section 13).

---

## 4. Postgres schema

One database `magpie` (role `magpie`) on its own CloudNativePG Cluster `magpie-pg` (section 18.2). Timestamps are
`timestamptz`. Migrations are plain SQL files applied at start under `pg_advisory_lock(hashtext('magpie.migrate'))`,
one transaction each, recorded in `schema_migrations`; shipped migrations are never edited, and each migration must be
readable and writable by the previous release (expand, then contract in a later release) because rollouts run old
and new replicas together.

```sql
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
                    ('admitted','preparing','starting','running','stopping','parked','completed','failed','canceled')),
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
  resumed_at        timestamptz,   -- a parked turn's answers admitted it again (8.1a): the driver counts from here
  parked_ms         bigint,        -- working time before it was parked (added to the end's duration_ms)
  parked_asks       text[],        -- the asks a parked turn waits on
  answer_tool_uses  text[],        -- the calls the resumed turn's answer line answers, until that line is seen (6.5)
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
```

Postgres TOAST compression for `cc_lines.stub`, `events.data`, `rows.item`, `blob_staging.bytes` and
`cc_pages.bytes` is set to `lz4`. The importer's own tables (`import_sources`, `import_shares`, `import_control`) are
cutover spec 7.3's, shipped as magpie migrations and dropped after the switch.

Seq allocation is always `update conversations set head_seq = head_seq + $n … returning head_seq` (events) and
`row_head = row_head + $m` (rows) inside the writing transaction, after `select … for update` on that row. The row
lock serialises writers per conversation, so both counters are contiguous, gap-free and never reused. The same lock
guards run_state, the lease, the epoch and quota accounting. Change-feed seqs are allocated the same way on the
`persons` row, always locked after the conversation row (one lock order everywhere: conversation, then person).

---

## 5. Blobs and the bucket

### 5.1 What becomes a blob

- Uploads (files and images a person attaches): stored raw, `kind 'file'` or `'media'`, `encoding 'raw'`.
- Strings extracted from Claude Code JSONL lines (section 6.4): stored as the exact bytes between the JSON quotes,
  `encoding 'base64-json'` (media: base64 text) or `'json-string'` (text, still JSON-escaped). Keeping the escaped
  bytes is what makes byte-exact reconstruction trivial.
- Sent-file snapshots: raw file bytes, `kind 'file'` or `'media'`, `encoding 'raw'`.
- Model-ready image copies made at upload (section 10.2): raw.

Blobs are content-addressed by SHA-256 of the stored bytes, so the duplicate image in a Read result
(`tool_result…source.data` and `toolUseResult.file.base64`) is stored once, and an image a person attaches (its model
copy) and the same image in the transcript's echo line are the same object. A blob's public id (`blobId`) is its
SHA-256 in lowercase hex.

### 5.2 Bucket layout, and the write path that does not wait for the bucket

One private bucket per deployment (YYC: `yyc-ai-magpie`, Hetzner fsn1, section 18).

```
blobs/<first 2 hex>/<sha256 hex>        the stored bytes
thumbs/<sha256 hex>-512.webp            image thumbnail (long edge 512 px), made on first request
display/<sha256 hex>-2000.webp          display copy (long edge <= 2000 px) of a non-upload image, made on first request
```

Two write paths:
- **Off the agent's hot path** (pigeon uploads, the supervisor's sent-file PUTs, thumbnails, display copies): the
  bytes stream straight to the bucket (single PUT up to 64 MiB, multipart above) before the row commits. A PUT of an
  existing key is harmless (same bytes).
- **On the agent's hot path** (cc writes, section 6.10): a blob a write introduces is stored in `blob_staging` in the
  write's own Postgres transaction (`blobs.stored = false`), so the write is durable in Postgres alone and Claude
  Code's `write()` waits for one round trip and one commit, never for the object store. The uploader job (every
  replica, every 1 s, `for update skip locked`, 4 in flight per replica) PUTs each staged blob, then in one transaction
  sets `stored = true` and deletes the staging row. A failed PUT backs off (1 s doubling to 5 min) and logs `blob
  upload failing` every 10 min. While staged, reads serve the bytes from `blob_staging`. If staging holds more than
  2 GiB (the bucket has been unreachable for long), cc writes that introduce a new blob answer 503 `unavailable`
  (retried by the supervisor) until it drains; writes without new blobs are unaffected.

Reads go through an in-process LRU (256 MiB per replica) keyed by sha.

### 5.3 References and lifetime

`blob_refs(owner, blob)`: one row per (conversation | share snapshot | unattached upload, blob). Inserting a reference
also bumps `blobs.last_ref_at` with `insert … on conflict (sha256) do update set last_ref_at = now()`, which takes the
blob row lock. The GC job (section 15) deletes a blob only when it holds that row lock, no reference exists, the blob
is `stored` and `last_ref_at < now() - 24 h`; then it deletes the object and its thumbnail and display copy. A weekly
reconcile lists the bucket and deletes objects older than 24 h that have no `blobs` row (a PUT whose transaction then
failed).

Deleting a conversation deletes its references; the GC frees what nothing else uses. There is no bucket trash or
versioning; Postgres backups cover the rows, not the bytes (section 18.6).

### 5.4 Reading blobs

- pigeon: `GET /v1/blobs/{blobId}?variant=thumb|display|full` (M15). No person header: pigeon signed the URL that led
  here and checked the signature (pigeon 5.9), and the blob id is a 256-bit content hash. magpie answers 404
  `not_found` unless the blob exists and at least one `blob_refs` row references it (a purged conversation's
  unshared blobs disappear at once for readers, before the GC frees the bytes).
  - `full`: the original bytes. Media stored as `base64-json` are decoded; `Content-Type` = the blob's mime. Text
    blobs are JSON-unescaped to UTF-8, `text/plain; charset=utf-8`.
  - `display`: an upload original → its model copy (`uploads.model_blob`, long edge <= 2000 px). Any other media blob
    → itself when it is a model-native type within 2000 px, else a WebP fitted to 2000 px made on first request and
    kept under `display/`.
  - `thumb`: WebP, long edge 512 px, made on first request and kept under `thumbs/`.
  - `display` and `thumb` of a non-image: 415 `unsupported_type`.
- supervisor: `GET /v1/sandbox/blobs/{sha}?as=raw|media|text` needs a reference by the sandbox's conversation
  (`raw` = the stored bytes; `media` = base64-decoded; `text` = unescaped).
- Shares and conversations read blobs only through these two routes; files panel downloads use 11.3/12.3.

Headers: `cache-control: private, max-age=31536000, immutable` and `etag: "<sha256 hex>"` (content-addressed),
`x-content-type-options: nosniff`, `content-security-policy: default-src 'none'; sandbox`, `content-disposition:
inline` for images and PDF, `attachment` with an RFC 5987 `filename*` otherwise.

### 5.5 Previews

- Media: dimensions are read from the header bytes at ingest (no decode) and stored on the blob row. Thumbnails and
  display copies are made lazily (decode, orient, resize with Lanczos3, WebP quality 80). At most 4 decodes run at
  once per replica; a decode over 40 megapixels answers 415.
- Text: at ingest magpie unescapes the first 16 KiB and keeps the first 4,000 characters as `preview` and the line
  count as `lines` inside the event's blob reference. The full text is fetched on tap (`variant=full`).

---

## 6. Claude Code state behind the FUSE layer

### 6.1 What lives here

The supervisor presents `~/.claude` as files. It keeps sandbox-local things local (sessions registry, shell
snapshots, `backups/`, `session-env/`, `.last-cleanup`, settings files) and sends everything that belongs to the
conversation to magpie, per the supervisor's scope table (supervisor 9.1): `projects/**` (the transcript
`<sid>.jsonl`, `<sid>/tool-results/*`, `<sid>/subagents/**`, workflow journals, `memory/`), `todos/**`, `plans/**`,
`tasks/**`, `file-history/**`. magpie is agnostic about which paths it gets; it stores whatever the leaseholder writes
under the conversation's scope, within the quota. New Claude Code paths (2.1.316 adds `.ccr-tip.json`,
`.precompact.json` and `storageV5` paths) need no magpie change: kind is decided by name (6.2), and only the main
transcript path produces events.

Because hermit gives one sandbox per conversation, each sandbox's view holds only its own conversation's files. The
per-person `~/.claude` of today (shared by all of a person's conversations and concurrent sandboxes) ends; nothing
collides across conversations, and Claude Code's auto-memory (`projects/-workspace/memory/`) becomes per
conversation, as Deyao decided (decision 6, 2026-11-02). Nothing is lost by this: the live auto-memory folders are
empty (inventory A8.F18), and if a file appears before the switch the importer copies the person's `memory/` into
each of their imported conversations (section 19.1), so nothing can be dropped.

### 6.2 File kinds and the main transcript

- `lines`: any path whose last component ends in `.jsonl` or contains `.jsonl.` (so Claude Code's compaction temp file
  `<sid>.jsonl.compact.tmp.<hex>` and a set-aside `<sid>.jsonl.superseded-<ms>` are lines files too). Stored as
  `cc_lines` rows plus a `tail`.
- `bytes`: every other file. Stored in `cc_pages` (256 KiB pages).
- `dir`: explicit directories (`mkdir`), so empty directories and `readdir` work.

The main transcript is a **path**, not a file: `main_transcript = 'projects/-workspace/' || provider_session_id ||
'.jsonl'`, set from the supervisor's `running` report (section 8.5: `transcriptPath`, checked to equal that form) and
by the importer. Only lines written at that path produce events:
- Lines written there before the path is known are stored without events and promoted (classified, given events in
  file order) in the transaction that sets `main_transcript`.
- A rename **away** from the main path (relocation, or 2.1.316's set-aside of an occupied destination: `<sid>.jsonl`
  → `<sid>.jsonl.superseded-<ms>`) turns the moved file into an ordinary lines file. Its lines keep their existing
  events (events are immutable, and the rows they made stay), it produces no new events, and its lines with events
  are entered in `cc_limbo` with reason `moved`.
- A file **created** at the main path (the fresh `<sid>.jsonl` written after a set-aside) is the main transcript. Its
  lines are matched against `moved` limbo of the scope by sha256 (in order first, then by hash, as 6.6 step 5): a
  matching line takes the old line's `event_seq` (no new event, no new row), an unmatched line is classified as new.
  `moved` limbo is never retracted (those lines still exist in the set-aside file); it expires after 10 min.
- A rename **onto** the main path (compaction's rename-over) is the rename-over matching of 6.6.

### 6.3 Line storage

Each complete line (ending in `\n`) is one `cc_lines` row: `stub` (the line with every extracted string's content
removed, so `"data":"iVBORw0…"` becomes `"data":""`), `spans` (`[[offset in stub where the content was, sha256], …]`),
`len`, `sha256` of the full reconstructed line, `start_off`, `ord`, `turn_id`, and `event_seq` if it became an event.
`turn_id` is the write's `turnId` parameter (section 16.2: the turn the supervisor was running when it read the FUSE
request) when that names a turn of this conversation, else the conversation's active turn, else null. So a line that
lands late (a slow magpie) is still credited to its own turn, never to the next one.

Bytes after the last newline are the file's `tail`. Claude Code's torn-tail repair (it prefixes the next append with
`\n` when the file doesn't end in one) needs nothing special: the `\n` completes the torn line, which is stored as an
unparsable internal line, byte for byte.

### 6.4 Extraction rules

`scan.rs` walks the line's bytes once, tracking JSON structure and key paths, and yields every string token with its
byte range (content only, without quotes). A string is extracted when:

1. Media: raw content length >= 1,024 bytes, only base64 alphabet characters, and the first bytes decode to a PNG,
   JPEG, GIF, WebP or PDF signature. Blob kind `media`, encoding `base64-json`, mime from the signature.
2. Text: raw content length >= 16,384 bytes, and the string is not an assistant line's `message.content[*].text` or
   `message.content[*].thinking` (assistant prose stays inline so a row never needs a blob to render a reply). Blob
   kind `text`, encoding `json-string`.

Everything else stays in the stub. A line that isn't valid JSON is stored whole (no extraction) up to the line cap.

Reconstruction of a line: walk `spans` in order, copy `stub[prev..offset]`, then the blob's bytes, then the rest.
Because the blob holds the exact escaped bytes, the output equals the input byte for byte. `sha256` is checked on
every reconstruction in tests and on a sample (1 in 1,000 lines) in production; a mismatch is logged as an error and
the read fails with 500 rather than returning wrong bytes.

### 6.5 Classification (main transcript only)

`classify.rs` parses the stub (valid JSON: extracted strings are empty strings) and decides:

| Line | Event kind | Notes |
|---|---|---|
| `type: "assistant"` | `cc.assistant` | `meta.toolUses` (ids), `meta.error` when the text matches `^API Error:` (section 8.9), `meta.sidechain`; updates `conversations.model` and `context_tokens` from `message.model` / `message.usage` when not a sidechain line |
| `type: "user"` containing `tool_result` blocks | `cc.user` | `meta.results: [{toolUseId, toolUseSeq}]`; fills `tool_links.result_seq` |
| `type: "user"`, the first non-`isMeta` prompt line written during a person-origin turn whose echo is not yet matched | none (internal) | the echo of `user.sent`; `turns.echo_matched = true` |
| `type: "user"` whose text starts with `<task-notification>` | `cc.user` | `meta.taskNotification = {toolUseId, status, summary}` parsed from the block |
| during a resumed parked turn (8.1a) before its answer line: a `type: "user"` line whose every `tool_result` starts with `[Tool call interrupted`, and a `type: "assistant"` line with `message.model` `<synthetic>` and no `tool_use` | none (internal) | Claude Code 2.1.304 closing the question its predecessor left open ("[Tool call interrupted: the session ended …]", then "No response requested.", supervisor 7.6) |
| in any turn: a `type: "assistant"` line with `message.model` `<synthetic>` whose content is exactly the text "No response requested." (a string or one text block) | none (internal) | Claude Code's placeholder reply when it resumes a conversation whose last turn ended without one; the old stack's history never showed it (`isNoResponsePlaceholder`; cutover 10.2 R6) |
| `type: "user"` text lines, after the echo rule, whose text starts with `<system-reminder>`, `Caveat:`, `[Request interrupted` or `Base directory for this skill:` | none (internal) | what the old stack's history left out (`INTERNAL_CONTENT_PREFIXES`): the interruption banner, reminders, caveats, skill bodies (cutover 10.2 R6) |
| `type: "user"` with `isCompactSummary` | `cc.user` | `meta.compactSummary = true`; not a prompt and never an echo (rule 2a shows it) |
| `type: "system"`, `subtype: "compact_boundary"` | `cc.user` | `meta.compaction = {trigger, preTokens, postTokens, durationMs}` from `compactMetadata` (`compact_metadata` live), 0 when absent; no text (rule 2a shows it) |
| during a resumed parked turn, its first prompt line (the supervisor's message with the person's answers) | `cc.user` | `meta.askAnswer = true`, `meta.results` = the answered calls (`turns.answer_tool_uses`, then cleared); not a prompt |
| other `type: "user"` lines that are not `isMeta` | `cc.user` | `meta.prompt = true`, `is_prompt = true` (prompts not sent through magpie, e.g. imported ones) |
| `custom-title` / `ai-title` records | none (internal) | if `name_source` is null, the title becomes this text (stays provisional), with `title.changed` |
| everything else (`api-request*`, `attachment`, `atis-latch`, `cost-state`, `last-prompt`, `mode`, `queue-operation`, other `system:*`, `isMeta` users, unparsable lines …) | none (internal) | stored for byte-exactness only |

An event's `data` is the parsed stub with each extracted string replaced by a blob reference object:

```json
{"$blob": {"blobId": "9f…", "bytes": 1398102, "kind": "media", "mime": "image/png", "width": 1600, "height": 1200}}
{"$blob": {"blobId": "c1…", "bytes": 48211, "kind": "text", "preview": "first 4,000 characters…", "lines": 912}}
```

`text` holds the plain text of user and assistant text blocks (not tool results), capped at 64 KiB, for search and
prompt previews. The classifier's input set is pinned per Claude Code release by the CI gate of section 20.

### 6.6 Operations and the rewrite algorithm

Claude Code does not only append: tombstone removal opens the file `r+`, `ftruncate`s at the start of a line and
`pwrite`s the tail back (or rewrites a file of up to 50 MiB whole); compaction writes `<file>.compact.tmp.<hex>` and
renames it over the original; torn tails get a `\n` prefix; relocation renames a transcript and its folder, or sets
an occupied destination aside (6.2). magpie supports all of it with one algorithm over the affected suffix of a lines
file.

For an operation with cut point `C` (write at offset `C`; truncate to `C`; replace-all has `C = 0`):

0. A write whose range `[C, C + len)` lies inside the file and already holds exactly the written bytes is answered
   200 with the current `{size, version}` and changes nothing (checked before every other rule, `offset_mismatch`
   included). This is how the sentry's resend of a request after a checkpoint (supervisor 9.3, WP-07 gate item 3:
   the same WRITE, same offset and bytes, under a new `unique` and so a new `opId`) applies once when it races the
   original; the supervisor's resend rule (keyed on the calling thread, 9.3) covers the other operations.
1. `C > size` is refused (409 `offset_beyond_eof`; Claude Code never writes past the end; the supervisor's repair is
   in 6.12).
2. Find the first line `L` with `start_off + len > C` (or the tail if `C` is past the last newline).
3. Fast path: a write at `C = size` with an empty tail splits the new bytes into lines and appends them. This is the
   normal case and touches nothing else.
4. Otherwise the old suffix is the bytes from `L.start_off` to the end (lines `>= L` and the tail). The new suffix is
   the old suffix with the write applied at `C - L.start_off` (overwrite, extending if it runs past the end), or cut at
   `C` for a truncate.
5. Split the new suffix into complete lines and a new tail. Match each new line against the old suffix lines by
   `sha256`, in order first, then by a hash lookup (moved lines), then against the file's `truncated` limbo. A
   matched line keeps its stub, spans and `event_seq`; unmatched lines are extracted and classified as new lines.
6. Delete the old suffix rows; insert the new rows with fresh `ord`s and recomputed `start_off`.
7. Old visible lines that no new line reused: a truncate puts them in limbo (6.7); an overwrite retracts them at once.
8. Update `size`, `tail`, `version + 1`, `mtime_ns`, `last_op_at`, and the conversation's `cc_bytes`.

This runs line by line and never holds the whole suffix in memory; only the per-line cap applies.

Rename of a lines file over another lines file (compaction) is the same matching with the source's lines as the new
content of the target: target lines with a matching sha in the source keep their events, the source's rows move to
the target's `file_id`, and the source path disappears; target lines with events that the source does not contain are
retracted. Rename to a free path changes `path` only (plus the main-path rules of 6.2). Rename of a directory renames
every path under it in one transaction.

### 6.7 Retraction and limbo

When lines with events leave the main transcript, readers must be told. To avoid churn from Claude Code's
truncate-then-rewrite patterns (which would otherwise retract the tail and re-add it), lines removed by a truncate go
into `cc_limbo` (reason `truncated`). A later write whose new line matches a limbo sha takes that line's `event_seq`
back (nothing is emitted).

Limbo is debounced, never flushed in the middle of a rewrite: it is flushed into one `retract` event `{"seqs":
[...]}` only when the file has had no operation for 10 s (`last_op_at`), or at the end of the turn the truncate
belonged to (the end report, 8.5), whichever comes first. A rewrite sequence (a truncate followed by writes at
increasing offsets, up to 50 or more 1 MiB writes for a 50 MiB whole-file rewrite) keeps pushing `last_op_at`
forward, so nothing is retracted while it progresses, however slow magpie is under load. The sweeper (section 15)
performs the quiet-file flush. Lines dropped by an overwrite or a rename-over are retracted at once. A new line whose
`uuid` equals a retracted line's `uuid` carries `meta.replaces = <old seq>`.

Retraction never deletes event rows; events stay immutable and the log stays append-only. Its effect on the timeline
(rows removed, a new epoch) is section 7.3 rule 9.

### 6.8 Byte files and directories

- Write: read-modify-write of the touched 256 KiB pages in one transaction; a write past the end zero-fills.
- Truncate: drop pages past the new size, trim the last one; extending zero-fills.
- Unlink: deletes the file's rows. The main transcript is protected: unlink answers 403 `protected` (EPERM). This
  stops Claude Code's age-based cleanup from deleting history; the supervisor also sets `cleanupPeriodDays` high, but
  magpie does not depend on that.
- `mkdir` creates a dir entry (parent must exist, 409 `exists` if present); `rmdir` refuses a non-empty directory
  (409 `not_empty`). Writes into a missing parent answer 404 (ENOENT); Claude Code then runs `mkdir -p` and retries.
- `link(2)` is not offered: the supervisor answers EPERM (supervisor 9.3) and Claude Code takes its write fallback.
- `fsync` needs no call: every acknowledged operation is already durable.

### 6.9 The lease (one writer per conversation)

`POST /v1/sandbox/lease` gives the calling sandbox a fresh `leaseId` and bumps `lease_epoch`, but only when the
caller's `Aviary-Sandbox-Id` is the conversation's live sandbox:
1. If it equals `conversations.hermit_sandbox` (from hermit's feed, 8.7) and `hermit_state` is `starting` or `ready`:
   grant.
2. Else magpie asks hermit `GET /v1/conversations/{id}/sandbox` (no wait, 2 s timeout). If hermit's live sandbox is
   the caller (state `starting` or `ready`): record it and grant. If hermit names another sandbox, or none: 409
   `not_current_sandbox`. If hermit is unreachable: 503 `unavailable` (the supervisor retries; it never takes a lease
   on missing evidence).
3. A migrating conversation: 409 `conversation_migrating`.

Granting makes any earlier lease dead at once. Every state-changing cc call and the command poll must carry the
current lease, or get 409 `lease_lost` (the supervisor answers EIO and stops). On `not_current_sandbox` the supervisor
stays unready and polls the lease every 30 s without restarting (supervisor 4.3), so two live sandboxes for one
conversation (a migration recorded as failed while the target runs, a host judged lost but not yet terminated while
the gateway's 5 s token cache still admits it) cannot steal the lease from each other in a loop: only hermit's
current sandbox ever holds it. A keeper-restarted supervisor in the same sandbox just takes a new lease. A hermit
`gone` event clears the lease. A live migration keeps the sandbox id and the lease; a pause keeps the lease.

The lease response also carries `resync` (section 6.12) and is the moment magpie clears `cc_resync`.

### 6.10 Durability and ordering of an acknowledged write

The supervisor answers the write syscall only after magpie's 200. magpie answers only after one Postgres transaction
committed: conversation row lock, lease and idempotency check (`cc_ops`), quota check, blob rows, staged bytes for new
blobs (5.2) and refs, `cc_lines` changes, events, rows, `tool_links`, file metadata, `head_seq`/`row_head`, change-feed
entries, NOTIFY. Commit returns after a synchronous standby has the WAL (section 18.2), so an acknowledged write
survives the loss of the primary. Nothing on this path waits for the bucket.

Operations carry `opId` (UUID). A replay of a committed `opId` returns the stored result without doing anything.
An append also carries the expected offset, so a stale retry after a lost response cannot double-write; a write
whose bytes are already at that offset is a success without a change (6.6 rule 0).

Acceptance numbers (section 20): p99 of a cc write carrying a 1 MiB image line <= 150 ms and p50 of a 10 KiB append
<= 15 ms, measured at magpie's handler inside the cluster with synchronous replication on. CI does not check them as
times (a shared runner measures itself, Deyao 2026-11-25): it checks the write's database work, one transaction of the
same statements wherever the write lands (crates/magpie/tests/ccfs_counts.rs), and ccfs_perf.rs prints the latencies.

### 6.11 Read path

- `GET /v1/sandbox/cc/tree` returns every path with kind, size, `mtimeNs`, mode and `version`, plus `ccBytes` and
  `quotaBytes` for STATFS; the supervisor caches it and serves `stat`/`readdir` locally (it is the only writer).
- `GET /v1/sandbox/cc/file?path=&offset=&length=` streams reconstructed bytes. For a lines file magpie seeks with
  `cc_lines_off`, then streams lines in `ord` order, splicing blobs (from staging or the bucket), with 16 blobs
  prefetched concurrently. Measured target: an 87 MB transcript (today's largest; about 50 MB of distinct blobs)
  streams in under 5 s from the cluster.
- The supervisor keeps a local copy of each file it reads and updates it with its own writes, so `--resume`,
  `pread(size-1)` and the 64 KiB tail read for tombstones are local reads. On a new lease it compares `version`s
  from the tree and refetches only changed files.

### 6.12 Quotas, errno mapping, unavailability and repair

Per conversation, `cc_bytes` counts stubs, pages and the size of each blob the first time this conversation
references it. Limits are in section 17. The supervisor maps answers to errno (supervisor 9.5):

| magpie answer | Supervisor |
|---|---|
| 404 `not_found` | ENOENT |
| 403 `protected` | EPERM |
| 409 `exists` / `not_empty` | EEXIST / ENOTEMPTY |
| 409 `lease_lost` | EIO, and the supervisor stops the agent |
| 409 `offset_mismatch` / `offset_beyond_eof` | the repair below |
| 413 `too_large` (line or file over its cap) | EFBIG |
| 507 `quota_exceeded` | EDQUOT |
| 400 `bad_request` | EINVAL |
| 503 `unavailable`, a connection error, a timeout, a gateway 502/504 | retried with the same `opId` (0.2 s doubling to 10 s); see the rule below |

The retry rule, the same in both specs (platform R26):
- **General unavailability** (magpie or its database is down for everyone: the supervisor's heartbeat and other calls
  fail too): retried for as long as it takes while the lease is valid. Claude Code's write blocks; nothing is dropped,
  nothing returns EIO.
- **One stuck operation** (the same op keeps failing for 10 min while the supervisor's heartbeat to magpie succeeds):
  the supervisor answers that write EIO, reports the turn `failed` with code `cc.write_stuck` (message: path, offset,
  the last error), and ends the generation. magpie, on that report, sets `conversations.cc_resync = true` and logs
  `cc write stuck` with the path. The next lease answers `resync: true`; the supervisor then refetches the tree and,
  for every file whose cached copy is longer than magpie's and whose first `magpie.size` bytes equal magpie's, appends
  the missing suffix `[magpie.size, local.size)` with fresh `opId`s before starting Claude Code. Staff see an error row
  with the code and can send again; the history is repaired on that send.

The offset repair (supervisor 9.5 item 6 becomes this): on `offset_mismatch` or `offset_beyond_eof` the supervisor
refetches the tree. If magpie's file is shorter than the local copy and magpie's bytes are a prefix of the local copy
(checked by reading magpie's last 64 KiB and comparing), it re-sends `[magpie.size, local.size)` with fresh `opId`s
and then retries the operation. Only when the local copy disagrees with magpie's bytes does it log an error, answer
EIO and stop the agent (a forked history must not continue). With synchronous replication this path is not expected;
it exists so a restore or a failover that lost an acknowledged write repairs itself from the supervisor's
write-through cache.

### 6.13 Failure modes

| Failure | What happens |
|---|---|
| Postgres primary lost | CNPG promotes a synchronous standby (no acknowledged write lost); writes answer 503 for the seconds of the failover and the supervisor retries, so Claude Code's append blocks briefly (no lost entries, unlike today's log-and-drop). Long-polls answer 503; pigeon shows reconnecting. |
| Planned Postgres restart (upgrade, node drain) | a switchover to a standby first (`primaryUpdateMethod: switchover`), seconds of 503 as above, never a stopped primary waiting for its node |
| Bucket down | cc writes are unaffected (staging); uploads, sent-file PUTs, thumbnails, display copies and downloads of unstaged blobs answer 503. Timeline pages are unaffected (rows hold references). |
| A replica dies mid-write | The transaction rolls back; the supervisor's retry with the same `opId` lands on another replica. |
| Reconstruction sha mismatch | 500 on that read and an error log with the file, ord and sha; never silent wrong bytes. |
| Two sandboxes for one conversation | only hermit's current sandbox gets the lease (6.9); the other gets `lease_lost` on its next call and `not_current_sandbox` on re-lease |

---

## 7. Events, timeline rows, live frames, the change feed

magpie keeps three ordered things per conversation and one per person (decision of review round 1, platform R27):
- **Events** (7.1-7.2): the immutable log, `seq` contiguous per conversation, every kind. The record; what the
  importer writes, what admin reads, what the projection is built from.
- **Timeline rows** (7.3-7.4): the protocol-neutral projection of events into what a conversation shows, written in
  the same transaction as the event, under the same lock, with their own dense `rowSeq` and a stable `epoch`. This is
  the per-conversation stream the target design names ("pigeon streams … by seq; a reconnect resumes by seq; history
  `before=<seq>&limit=40`, cost ~ page size"): pigeon reads rows, not events, because a stateless multi-replica pigeon
  cannot do stateful projection (pairing a tool result with its call, folding task notifications, deciding what is a
  row, renumbering densely).
- **Live frames** (7.5): ephemeral partial assistant text of a running turn, superseded by the durable row.
- **The change feed** (7.6), per person: list changes plus turn, sandbox, ask and timeline-replaced entries.

### 7.1 The event object

```json
{
  "seq": 812,
  "kind": "cc.assistant",
  "at": "2026-11-02T03:46:12.345Z",
  "turnId": "0190f6c2-…",
  "data": { },
  "meta": { },
  "viewer": null
}
```

`seq` starts at 1 per conversation and is contiguous. Events are immutable. Clients must ignore unknown fields and
skip unknown kinds; a new kind ships only after its projection rule exists (7.3). `data` of `cc.*` events is the
transcript line as written by Claude Code (its own snake_case keys are data, not API fields, and are kept as is).

### 7.2 Kinds

| Kind | Written by | `data` |
|---|---|---|
| `conversation.created` | magpie | `{"title": "…"|null, "origin": {"kind": "new"|"continue"|"import", "shareId"?: "…"}}` |
| `user.sent` | magpie (pigeon's send) | `{"clientMessageId": "…", "text": "…", "files": [Attachment], "images": [Image]}` |
| `turn.started` | magpie | `{"turnId": "…", "origin": "person"|"agent"|"import"}` |
| `sandbox.preparing` | magpie (driver, after 1 s of acquisition) | `{}` |
| `sandbox.ready` | magpie | `{}` |
| `turn.stop_requested` | magpie | `{"turnId": "…"}` |
| `turn.ended` | magpie | `{"turnId": "…", "outcome": "completed"|"failed"|"canceled", "error"?: {"code": "…", "message"?: "…", "detail"?: "…"}}` |
| `title.changed` | magpie | `{"title": "…", "nameSource": null|"title"|"user"}` |
| `files.sent` | magpie (sent-file snapshot) | `{"toolUseId": "…", "toolUseSeq": 640|null, "caption": "…"|null, "files": [SentFile]}` |
| `task.status` | magpie (supervisor report) | `{"taskId": "…", "toolUseId": "…"|null, "event": "started"|"progress"|"updated"|"notification", "status": "running"|"completed"|"failed"|"stopped", "description"?: "…", "summary"?: "…", "outputFile"?: "…"}` |
| `ask.pending` | magpie (supervisor report) | `{"askId": "…", "toolName": "AskUserQuestion", "toolUseId": "…"|null, "input": {…}}` |
| `ask.resolved` | magpie | `{"askId": "…", "resolution": AgentPermissionResponse}` (a cancel is `{"behavior": "deny", "message": "canceled"}`) |
| `retract` | magpie (cc rewrite) | `{"seqs": [431, 432]}` |
| `cc.user`, `cc.assistant` | the transcript (section 6.5) | the transcript line, blobs as references |

Shared shapes:

```json
Attachment = {"uploadId": "uuid", "name": "Q3 report.pdf", "mimeType": "application/pdf", "size": 812345,
              "path": "uploads/Q3 report.pdf", "blob": {"$blob": {…}}}
Image      = {"uploadId": "uuid", "mimeType": "image/jpeg", "size": 2211234, "width": 4032, "height": 3024,
              "blob": {"$blob": {…}}, "modelBlob": {"$blob": {…}}}
SentFile   = {"path": "report/q3.xlsx", "name": "q3.xlsx", "size": 48211, "mimeType": "application/vnd…",
              "isImage": false, "blob": {"$blob": {…}}}
```

`meta` keys (all optional): `prompt`, `taskNotification`, `toolUses`, `results`, `error`, `replaces`, `sidechain`,
`askAnswer` (6.5), and, on imported events only, `displayText` and `attachments` (section 19.3).

Events are read by admin routes (`/v1/admin/conversations/{id}/events?before|after|tail&limit`, page shape
`{events, headSeq, hasOlder, hasNewer}`), by the projection, and by the importer's verification. pigeon does not read
events.

### 7.3 Timeline rows and the projection

```text
Row {
  seqStart: int,            // the row seq of the row's first version (its identity)
  seqEnd: int,              // the row seq of this version
  sourceSeqRanges: [{startSeq, endSeq}],  // the row seqs of every version of this row so far, as ranges
  updated: bool,            // seqEnd != seqStart (a result, a status or a sent file changed it)
  timestamp: iso, turnId?: uuid,
  item: RowItem
}
RowItem =
  {kind: "user", messageId, clientMessageId?, text, images: [{blobId, mimeType, width?, height?}],
   files: [{path, name, mimeType, size, missing?}], viewer?: email}
| {kind: "assistant", messageId, blockIndex, text}
| {kind: "reasoning", messageId, blockIndex, text}
| {kind: "tool", callId, messageId, blockIndex, name, input, status: "running"|"completed"|"failed"|"canceled",
   result?: {text, truncated, bytes, blobId?, isError, images?: [{blobId, mimeType}]},
   sentFiles?: [{path, name, size, mimeType, isImage}], task?: {taskId, status, summary?}, error?: ErrorInfo}
| {kind: "error", code, message, detail?}
ErrorInfo = {code, message, detail?}
```

These are pigeon's shapes (pigeon 7.8 and 14.1.0), with `messageId`/`blockIndex` added on model rows so live frames
can be superseded (7.5). pigeon's mapping to Paseo is a pure function of one Row.

The projection (`rows/`) runs inside every event-appending transaction, under the conversation row lock, after the
event insert. Each new row and each row update takes the next `rowSeq` (`row_head + 1`), recorded in `row_seqs`.
Rules, in event order:

1. `user.sent` → new `user` row: `messageId = clientMessageId`, `text`, `images` (original blob ids; pigeon signs
   `display`/`thumb` URLs for them), `files` (the attached workspace paths), `viewer` when sent while viewing as.
   `is_prompt`.
2. `cc.user` with `meta.prompt` → new `user` row: `messageId` = the line's `uuid`, text = `meta.displayText` or the
   text blocks, images = the line's image blocks (blob ids), files = `meta.attachments`. `is_prompt`.
   2a. `cc.user` with `meta.compaction` → new `assistant` row, `messageId` = the line's `uuid`, `blockIndex` 0, text
   the old stack's sentence: `Compacted · <trigger>`, then `<pre> → <post> tokens` (or `<pre> tokens` without a post
   count) when there is a pre count, then the duration, joined by ` · ` (tokens as `999`, `155k`, `1.3M`; durations as
   `4s`, `1m 3s`: `claude-sessions.provider.ts` `formatTokenCount`/`formatDuration`, JavaScript's rounding).
   `cc.user` with `meta.compactSummary` → new `assistant` row with the summary's text (none when blank). Not
   `is_prompt`. This is how the old stack showed a compaction (cutover 10.2 R6).
3. `cc.user` with `meta.taskNotification` → no new row. The tool row with that `toolUseId` (the background launch) is
   updated: `task = {taskId, status, summary}`, `status` = completed / failed / canceled (stopped, killed) per the
   notification, and `result.text` = the summary when the launch's own result was only "started". This is today's
   history folding (inventory A9.F13), now done once, at write time. Unknown `toolUseId` → no row.
4. `cc.assistant` (not sidechain) → one row per content block, `messageId = message.id`, `blockIndex` = the block's
   index within that message (Claude Code writes one line per block, in order; magpie counts earlier lines with the
   same `message.id`): `text` (not blank) → `assistant`; `thinking` (not blank) → `reasoning`; `tool_use` → `tool`
   `{callId: id, name, input, status: "running"}` (with `sentFiles` already filled if a `files.sent` for that id
   exists). Text and thinking blocks that are empty or only whitespace (Claude Code's `"\n\n"` before a tool call,
   which the old stack's history left out) and `redacted_thinking` make no row. An `API Error:` assistant line is an ordinary
   `assistant` row (pigeon rewrites the text, pigeon 7.9).
5. `cc.user` with `tool_result` blocks → for each block, update the tool row with that `callId`: `status` completed,
   or failed when `is_error`; `result = {text: first 4,000 characters of the text content (a text blob's preview when
   extracted), truncated: the full text is longer, bytes, blobId: the text blob when extracted, isError, images: image
   blocks}`. No tool row → nothing (the call was in a sidechain or before an import cut). A `cc.user` with
   `meta.askAnswer` (a parked turn's answers, 6.5) updates the tool row of each `meta.results` call the same way, its
   text being the line's text, and makes no `user` row: the person sees their answer as the question's result, as when
   the question was answered in its first sandbox.
6. `files.sent` → update the SendUserFile tool row's `sentFiles`. If the tool row does not exist yet, nothing now;
   rule 4 picks the files up when the row is made.
7. `task.status` with a `toolUseId` → update that tool row's `task` and, for terminal statuses, its `status`.
8. `turn.ended` → every tool row of that turn still `running` is updated to `canceled` (outcome canceled) or `failed`
   (outcome failed; `completed` leaves them, background launches stay running until their notification). Outcome
   `failed` also makes a new `error` row `{code, message, detail}` (codes in 8.9; `message` is English for logs,
   pigeon localises by code). Outcome `canceled` makes no row.
9. `retract` → every row with a source event in `seqs` is affected. A row whose identifying event (the user line, the
   assistant block, the tool_use) is retracted is deleted. A tool row that lost only its result or a later update is
   rebuilt from its surviving events and takes a new `rowSeq`. If any row was deleted or rebuilt, `epoch + 1` and a
   `timeline_replaced {epoch}` change (7.6). A retract that touched no row (internal lines only) changes nothing.
10. `conversation.created`, `turn.started`, `turn.stop_requested`, `sandbox.*`, `title.changed`, `ask.*`: no row;
    they reach pigeon through the change feed and the Conversation object.

Updates of a row (rules 3, 5, 6, 7, 8) change `item`, set `seq_end` to the new `rowSeq`, append the seq to
`sourceSeqRanges`, and keep `row_id`. Row seq density holds by construction: every `rowSeq` from 1 to `row_head` is
issued to exactly one row version. The epoch starts at 1, changes only under rule 9, and is the same on every replica
and across restarts and rollouts (it is a column). `rowSeq` continues across epochs (never reset, never reused); after
an epoch change pigeon refetches the tail (pigeon 7.6.3), so continuity across epochs does not matter to the app.

Imported conversations are projected by the same code as they are written (the importer links the magpie library),
so an imported conversation's rows equal the rows a live conversation with the same events would have.

### 7.4 Timeline paging and the long-poll (M9)

`GET /v1/conversations/{id}/timeline` with exactly one of:
- `tail=1`: the last `limit` rows by `seqStart`, ascending by `seqStart`.
- `before=S`: rows with `seqStart < S`, the last `limit` of them, ascending by `seqStart` (current versions).
- `after=S`: one entry per row seq `s` with `S < s <= row_head`, ascending by `s`, up to `limit`: the row that `s`
  belongs to (`row_seqs`), in its current state, with `seqEnd = s` and `updated = s != seqStart`. An entry whose row
  was later updated again is still sent at its own `s` (with the current item), so a live reader sees every integer
  exactly once and never a false gap; the app's seq gate (`seq == endSeq + 1`) accepts every entry. Seqs of rows
  deleted by a retract are skipped (their epoch is gone; the response's new `epoch` tells pigeon to reset).
- `limit` 1-500, default 40. `waitMs` 0-25000, only with `after`: if no entry with `s > S` exists, hold the request
  until one does, until live frames of the running turn change (7.5), or until `waitMs` passes (then `rows: []`).

Response:

```json
{
  "epoch": "3",
  "minSeq": 1, "maxSeq": 812,
  "hasOlder": true, "hasNewer": false,
  "rows": [Row],
  "live": LiveState | null
}
```

`epoch` is a decimal string. `minSeq` = the smallest `seqStart` among the conversation's rows; `maxSeq` = `row_head`
(so `maxSeq + 1` is the next live seq). `hasOlder` / `hasNewer` say whether rows exist before / after the returned
window. A response stops early at 4 MiB of JSON. Cost is proportional to the page: rows come from the primary key and
the `rows_end` index, blobs stay references. A deleted (Recently deleted) conversation is readable. `live` is 7.5.

Long-poll mechanics: the handler subscribes to the conversation's watch (section 14) first, then queries; on a wake-up
or every 1 s (a missed NOTIFY costs at most 1 s) it queries again. No database connection is held while waiting. A
draining replica answers held polls at once with what it has (possibly empty); the next poll lands on another
replica and resumes by seq, so drains finish in under 1 s and no stream is ever cut mid-frame (NDJSON streaming is
not offered; pigeon 8.1).

`GET /v1/conversations/{id}/prompts` (M10) → `{"epoch", "prompts": [{"seq": <row seqStart>, "timestamp", "preview":
<first 200 characters>}]}` over `is_prompt` rows.

`GET /v1/conversations/{id}/search?q=&cursor=&limit=50` → `{"epoch", "locations": [{"seq": <row seqStart>, "kind",
"count"}], "nextCursor"}` (case-insensitive substring over the events' `text`, mapped to rows). pigeon may light up
`agent.timeline.search` with it later; nothing depends on it now.

### 7.5 Live frames (token streaming)

The supervisor sends partial assistant output of a running turn (supervisor 8.3):
`POST /v1/sandbox/turns/{turnId}/live` (lease) `{"frames": [{n, messageId?, index?, op, block?, name?, offset?,
text?, parentToolUseId?, status?, at}]}` → 204.

- Storage: `live_frames` (unlogged). Insert `on conflict (turn_id, n) do nothing`, so a re-sent batch is idempotent;
  a `delta` whose `(messageId, index, offset)` repeats bytes already stored is also ignored. A turn that is not the
  conversation's active turn answers 409 `turn_not_active` (the supervisor drops the frames; never retried).
- NOTIFY `magpie_live` `<conversation>:<n>` after commit; held M9 polls of that conversation wake.
- Supersede: rule 4 of 7.3, when it makes the durable row for `(messageId, blockIndex)`, deletes that block's live
  frames in the same transaction, so no reader can see the block both live and durable in one response after that
  commit. pigeon replaces its in-progress row with the durable row keyed by `(messageId, blockIndex)` (requirement in
  21.2).
- Cleanup: the sweeper deletes a turn's frames 60 s after `turn.ended` (a block the turn never completed, e.g. an
  interrupted reply, disappears; pigeon drops in-progress rows at the turn's end). After a magpie crash Postgres may
  truncate the unlogged table: live text then resumes from the next frame (readers see `reset: true`); the durable
  rows are unaffected.

`LiveState`, included in an M9 response whenever the conversation has an active turn with stored frames:

```json
{"turnId": "…", "n": 122, "reset": false,
 "blocks": [{"messageId": "msg_01H…", "blockIndex": 0, "block": "text", "name": null,
             "offset": 0, "text": "今日港珠澳大橋…", "done": false, "afterRowSeq": 811}],
 "status": {"status": "api_retry", "attempt": 2, "retryInMs": 4000} | null}
```

- An M9 `after` poll may pass `liveAfter=<n>`: `blocks` then carry only text added after frame `n` (`offset` = where
  it starts within the block). Without `liveAfter`, with a `liveAfter` from another turn, or after a `gap` frame or a
  truncation, `reset: true` and every open block is sent whole from offset 0.
- `afterRowSeq` = `row_head` when the block started, so pigeon places the in-progress row after that row.
- `GET /v1/conversations/{id}/live` returns the same object with `reset: true` (a socket that joins mid-message), or
  204 when no turn is active.

Live frames never consume a `rowSeq` and are never events: they cannot disturb seq density or history.

### 7.6 The change feed (M12)

`GET /v1/changes?after=<seq>|latest&limit=500&waitMs=25000` with `Aviary-Person` →

```json
{"generation": "6a1e…", "headSeq": 1236, "changes": [Change]}
```

`Change = {seq, at, type, conversationId, …}`, `seq` dense per person, types and fields exactly pigeon 14.1:

| type | fields | written when |
|---|---|---|
| `conversation_upserted` | `conversation: Conversation` (its state when the change is read) | create, title change, run state or sandbox phase change, activity touch, soft delete (`deletedAt` set), restore (`deletedAt: null`), ask opened/closed |
| `conversation_removed` | `reason: "purged"` | purge (permanent delete, the 30-day job, a person purge) |
| `turn` | `turnId, phase: "started"|"completed"|"failed"|"canceled", origin: "person"|"agent", error?: ErrorInfo, timelineSeq` | `turn.started`, `turn.ended` |
| `sandbox` | `state: "preparing"|"ready"` | `sandbox.preparing`, `sandbox.ready` (and `ready` at the end of a turn that showed preparing) |
| `ask` | `ask: Ask` | `ask.pending` |
| `ask_resolved` | `askId, resolution: AgentPermissionResponse` | `ask.resolved` |
| `timeline_replaced` | `epoch` | 7.3 rule 9 |

- Every change is inserted in the same transaction as the event or state change it reports. `timelineSeq` is
  `row_head` at that commit: because the supervisor's end report waits for every transcript line of the turn (8.5),
  the turn's last row has a `rowSeq <= timelineSeq`, and pigeon releases `turn_completed` only after it (pigeon 8.4).
- `after=latest` answers `{generation, headSeq, changes: []}` at once. `limit` 1-500. `waitMs` as 7.4 (wakes on the
  person's watch, re-queries every 1 s).
- `generation` is the person's `change_generation`; it never changes (pigeon uses it as Paseo's list generation, so a
  reconnect never looks like a server restart).
- Retention 30 days; an `after` older than the oldest kept change minus 1 answers 410 `cursor_expired` and pigeon
  re-snapshots with M1.

---

## 8. Turns: admission, the driver, commands, reports, cancel, failure

### 8.1 State machines

Turn:

```
admitted ──(sandbox not ready after 1 s)──> preparing ──(hermit: ready)──┐
   │                                                                     v
   └────────────(hermit: ready within 1 s)───────────────────────> starting ──(supervisor: running)──> running
   any of admitted/preparing/starting/running ──(cancel)──> stopping (or canceled at once before delivery)
   running/stopping ──(supervisor: completed|failed|canceled, or a heartbeat that shows the end, 8.5)──> ended
   running ──(supervisor: parked, 8.1a)──> parked ──(the person answered every question it waits on)──> admitted
   parked ──(cancel)──> canceled
   admitted/preparing ──(hermit refuses, sandbox gone, or 15 min)──> failed
   starting ──(start_turn not acked 120 s after ready)──> failed (sandbox.unreachable)
   running ──(silence past its deadline and hermit says gone, or 5 min of silence while hermit says alive)──> failed
   stopping ──(no confirmation in 30 s)──> canceled (stop.forced) + kill command
```

Conversation `run_state` follows its active turn: `admitted`, `preparing`, `running` (turn starting or running),
`stopping`, then `idle`, or `held` when the supervisor reports background tasks after the turn ended. `held` goes to
`running` when an agent-started turn begins, and to `idle` when the supervisor reports no tasks or a cancel stops
them.

### 8.1a Parked turns (DECISIONS 17)

A question nobody answers never holds a sandbox. When a run waits only on its open questions (supervisor 7.6) and its
sandbox is released (hermit's idle release, a pool victim, a lifetime or forced release), the supervisor reports the
turn `parked` instead of ending it: `state = 'parked'`, no sandbox, no driver, **no deadline** (the sweeper and hermit's
feed leave it alone), `parked_asks` = its open asks, `parked_ms` += the report's `durationMs`. It stays the
conversation's active turn (`run_state` `running`, `Conversation.turn` set) and its asks stay in `Conversation.asks`,
so the person still sees the question; nothing about it ends or fails.

- **Answer** (M11) on an ask of the parked active turn: `ask.resolved` with the person's response and change
  `ask_resolved` at once (no supervisor holds the question). When no ask of the turn is open any more, the turn is
  **admitted again**: a new `start_turn` for the same turn with `resume.providerSessionId` and `answers[]` (each
  `{askId, toolName, toolUseId, input}` of its `ask.pending` with `{allow, updatedInput, message}` of its resolution;
  8.4), `state = 'admitted'`, `resumed_at = now()`, the admission deadline from `resumed_at` (8.8),
  `answer_tool_uses` = the answered calls (6.5), `run_state = 'admitted'`; the replica that took the answer starts the
  driver (8.3), which gets a new sandbox as for any admitted turn. A deny with `interrupt: true` ends the turn
  canceled instead. The person sees their answer, then the reply, a little later (a sandbox starting).
- **Cancel** (M8) of a parked turn: its open asks resolve canceled and it ends `canceled` at once.
- **The answer crossed the park:** an `answer_ask` queued for the old supervisor that the `parked` report finds
  (its supervisor had already let the question go) is taken as the person's answer: the command is canceled, the ask
  resolved, and the turn resumes as above. A `parked` report with no open ask of the turn ends it `failed`
  `sandbox.lost` (never expected: the ask's report precedes it in the turn's lane).
- `durationMs` of the turn's end adds `parked_ms` (both parts' working time, the waits left out).

### 8.2 Admission (`POST /v1/conversations/{id}/turns`, M7)

Request (pigeon 14.1 M7):

```json
{"clientMessageId": "draft-7f3a:initial-message", "text": "幫我整理今日有關港珠澳大橋嘅新聞",
 "images": [{"uploadId": "…"}],
 "files": [{"path": "uploads/0b4c…/Q3 report.pdf", "name": "Q3 report.pdf", "mimeType": "application/pdf", "size": 812345}]}
```

One transaction:
1. Lock the conversation row. 404 if not the person's. 409 `conversation_migrating` `{retryAfterMs}` if `migrating`
   (cutover 6.4; during the switch window the body also carries `route: "legacy"` and a fenced conversation's call
   waits ≤ 3 s for the release instead, DESIGN 9.4 and 12 D14). 409 `conversation_deleted` if `deleted_at` is set.
2. Idempotency: if a turn with this `clientMessageId` exists, return it (200, same body) without doing anything.
3. If `run_state` is `admitted`, `preparing`, `running` or `stopping`: 409 `turn_running`. (`held` is admitted; the
   supervisor supersedes the background work, supervisor 7.7.) This is the single admission point, so two sockets
   sending at once get exactly one turn.
4. Attach uploads: `images[].uploadId` and the upload id inside each `files[].path` (the opaque
   `uploads/<uploadId>/<name>` path M13 returned) must name the person's unattached uploads, within the turn limits
   (400 `too_many_attachments`). **Legacy paths** (until A7, DESIGN 9.5): a `files[].path` (or image path) with the
   old asset prefix (CloudCLI's `cloudcli-assets/` naming, as a composer loaded before the switch or a T0→gate upload
   holds it) is resolved, outside this transaction and before it starts, in order through `uploads.legacy_path` (the
   T0→gate tee), then `import_uploads` (the importer's ingestion), then, on a miss, a synchronous ingest of that one
   file from `users/<key>/claude/cloudcli-assets/` by the import Deployment (`POST /v1/ingest-upload`, read-only on the
   source, ≤ 5 s; available until `import-done`, which first runs a last full ingestion). A resolved legacy path gives
   a **fresh upload copy per send** (a new unattached upload with a new blob ref to the same blob), since CloudCLI
   reuses an asset path any number of times and one file may be sent to several conversations; that copy is what this
   step attaches. Only after all three miss: 400 `unknown_upload` (counted in
   `magpie_unknown_upload_total{legacy="true"}`, a switch zero counter). A non-legacy path or upload id that names no
   unattached upload of the person: 400 `unknown_upload`. The same rule applies to M3's initial message (9.2). Assign `workspace_path = uploads/<name>` unique
   within the conversation (`name (2).ext` on clashes, `/` and NUL replaced, leading dots replaced, at most 200
   characters), add conversation blob refs.
5. Append `user.sent` (with `viewer` while viewing as, plus a `view_as_log` row `send` in the same transaction) and
   `turn.started`; insert the turn (`admitted`, origin `person`, deadline per 8.8); insert command `start_turn`
   (8.4); if `name_source` is null and `title_attempts < 3`, insert command `title`; set `run_state = 'admitted'`,
   `active_turn`, `activity_at = now()`, `last_user_message_at`; changes `turn {phase: started}` and
   `conversation_upserted`; NOTIFY.

Response 202 `{"turn": {"id": "…", "startedAt": "…"}}` (200 for a repeat). The user's text is stored before anything
can fail, so a turn that later fails still shows as sent and unanswered (today's failed-turn table becomes ordinary
events and an error row).

### 8.3 The driver (sandbox acquisition)

The replica that admitted the turn claims it (`driver = pod name`, `driver_until = now + 30 s`, renewed every 10 s)
and calls hermit (hermit 3.6), with `MAGPIE_TOKEN_HERMIT`:

1. `PUT /v1/conversations/{id}/sandbox` `{"person": "…"}` (2.5 s timeout; idempotent per conversation):
   - 200 `Sandbox` (state `ready`) → record `turns.sandbox_id`, `ready_at`; turn `starting`; `sandbox.ready` if
     `sandbox.preparing` was emitted.
   - 202 `Sandbox` (state `starting`) → record `sandbox_id`; go to step 2.
   - 403 `not_allowed` → failed `sandbox.not_allowed`.
   - 409 `person_mismatch` → failed `internal` (error log `hermit person mismatch`; a magpie bug).
   - 503 `sandbox_busy` `{reason, retryAfterS}` → failed `sandbox.busy` with `detail = reason` and the retry time in
     the error (`retryAfterS`).
   - Other 5xx, a timeout or a connection error → retry with backoff (0.5 s doubling to 10 s) until the deadline.
2. `GET /v1/conversations/{id}/sandbox?waitMs=25000&afterVersion=<v>` (35 s timeout), repeated with the latest
   version, until `state` is `ready` (→ as 200 above) or `gone` (→ failed with the code mapped from `release.code`:
   `sandbox_busy` → `sandbox.busy`, `sandbox_failed` → `sandbox.failed`, `sandbox_lost` / `sandbox_released` →
   `sandbox.failed` before ready). 404 `no_sandbox` → back to step 1. Errors → retry as above.
3. The deadline (admission + 15 min) → failed `sandbox.timeout`.

There is no fixed-interval poll: the long-poll answers within milliseconds of hermit's change. If no answer that says
ready arrives within 1 s of admission, the driver appends `sandbox.preparing` (`run_state = 'preparing'`,
`sandbox_phase = 'preparing'`, change `sandbox {preparing}`). A failure appends `turn.ended` (failed, code), cancels
the turn's undelivered commands, sets `run_state = 'idle'`, `last_turn`, `sandbox_phase = null`, change `sandbox
{ready}` if preparing was shown.

If the replica dies, `driver_until` lapses and the sweeper on any replica claims the turn with
`for update skip locked` and continues from the turn's state (hermit's `PUT` is idempotent). A drain releases claims
at once (`driver_until = now()`).

Budget never comes from hermit: a model budget refusal arrives as Claude Code's `API Error: 402 …` line from egress
and is tagged `model.budget` (8.9).

### 8.4 Commands (the supervisor's work queue)

The supervisor long-polls through the gateway:

`GET /v1/sandbox/commands?after=<cmdSeq>&waitMs=25000` (with `Aviary-Lease`) → as soon as there is a command with
`cmdSeq > after` that is not canceled, or after `waitMs`:

```json
{"commands": [
  {"cmdSeq": 41, "type": "start_turn", "turnId": "…", "payload": {
     "text": "幫我整理今日有關港珠澳大橋嘅新聞",
     "files":  [{"uploadId": "…", "name": "Q3 report.pdf", "mimeType": "application/pdf", "size": 812345,
                 "workspacePath": "uploads/Q3 report.pdf", "blob": "<sha256 hex>"}],
     "images": [{"uploadId": "…", "mimeType": "image/jpeg", "workspacePath": "uploads/IMG_0042.jpg",
                 "blob": "<original sha>", "modelBlob": "<model copy sha>", "modelMime": "image/jpeg"}],
     "resume": {"providerSessionId": "279aabf0-…"|null},
     "sentAt": "2026-11-02T03:46:10.120Z",
     "answers": [ {"askId": "…", "toolName": "AskUserQuestion", "toolUseId": "…", "input": {…},
                   "allow": true, "updatedInput": {…}, "message": null} ]}},   // a parked turn's resumption only (8.1a)
  {"cmdSeq": 42, "type": "title", "payload": {"userPrompt": "Here is the first message …", "maxTokens": 100}},
  {"cmdSeq": 43, "type": "interrupt", "turnId": "…", "payload": {}},
  {"cmdSeq": 44, "type": "kill", "turnId": "…", "payload": {"reason": "stop"|"watchdog"}},
  {"cmdSeq": 45, "type": "answer_ask", "payload": {"askId": "…", "allow": true, "updatedInput": {…}, "message": null}},
  {"cmdSeq": 46, "type": "stop_tasks", "payload": {}}
], "head": 46}
```

Returned commands get `delivered_at`. The supervisor acks each with `POST /v1/sandbox/commands/{cmdSeq}/ack`
`{"result": {…}}`. A restarted supervisor polls with a smaller `after` and gets unacked commands again; commands are
idempotent per `cmdSeq` on the supervisor side. `start_turn` is created at admission, so a supervisor that boots picks
it up at once; it waits harmlessly while hermit prepares the sandbox. Acked commands older than 24 h are deleted by
the retention job.

The supervisor fetches blobs with `GET /v1/sandbox/blobs/{sha}?as=raw` and writes files and originals into
`/workspace/uploads` itself (supervisor 12.1); model copies become image blocks. magpie never writes the workspace.

### 8.5 Supervisor reports

All with `Aviary-Lease`. The supervisor retries every report on 5xx and connection errors for as long as its lease is
valid and never drops one (supervisor 11 changes accordingly, 21.2); the heartbeat reconciliation below is the
backstop for anything that still goes missing.

- `POST /v1/sandbox/turns/{turnId}/state`
  ```json
  {"state": "running"|"completed"|"failed"|"canceled"|"parked", "at": "…",
   "providerSessionId": "279aabf0-…", "transcriptPath": "projects/-workspace/279aabf0-….jsonl",
   "exitCode": 0, "error": {"code": "run.exited"|"run.error"|"run.oom"|"cc.write_stuck", "message": "…"},
   "held": false, "tasks": [{"taskId": "…", "toolUseId": "…", "type": "local_bash", "description": "…", "startedAt": "…"}],
   "lastUuid": "<uuid of the turn's last transcript message>"|null,
   "usage": {"inputTokens": 0, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0},
   "durationMs": 41230}
  ```
  Allowed transitions are those of 8.1; others answer 409 `bad_transition` with the current state (a replay of the
  same end is answered 200). `parked` (from `running`, a replay answered 200) is 8.1a. `running` keeps the turn's
  first `started_at` (a resumed parked turn's start is its admission's, so working time does not restart). `running` sets `started_at`, `provider_session_id` and `main_transcript` (promoting
  earlier lines, 6.2). An end report whose `lastUuid` is not yet stored as a main-transcript line answers 409
  `transcript_pending` (retried by the supervisor), so `turn.ended` can never precede the reply's last row; the
  supervisor already waits for its writes before reporting (supervisor 8.2, with no time cap now that writes never
  drop). An accepted end flushes the turn's limbo (6.7), appends `turn.ended`, makes the projection's end rows (7.3
  rule 8), sets `activity_at`, `last_turn`, the changes `turn` and `conversation_upserted`, stores `usage` and
  `durationMs` (informational only; cost stays egress's). `held: true` with tasks sets `run_state = 'held'` and
  `background`; else `idle`. `cc.write_stuck` also sets `cc_resync` (6.12).
- `POST /v1/sandbox/turns` `{"turnId": "<uuid v7 from the supervisor>", "origin": "agent", "at": "…"}` for a turn
  Claude Code started by itself (task completion, Monitor events, wake-ups): appends `turn.started` (origin agent),
  turn `running`, `run_state = 'running'`, change `turn {started, origin: agent}`. Idempotent per `turnId`. Its end is
  reported with the state route. Lines written in between carry its `turnId`.
- `POST /v1/sandbox/tasks` `{"events": [{"id": "<gen>:<stdout offset>", "taskId", "toolUseId", "event", "status",
  "description", "summary", "outputFile", "at"}]}` → `task.status` events (rule 7 of 7.3) and an updated
  `background`. `id` is recorded in `report_ids`; a repeated id is skipped.
- `POST /v1/sandbox/asks` `{"askId", "toolName", "toolUseId"?, "input"}` → `ask.pending`, change `ask`, the ask in the
  Conversation. `POST /v1/sandbox/asks/{askId}/resolved` `{"outcome": "answered"|"canceled"}` → `ask.resolved` (with
  the person's answer when answered, the cancel resolution otherwise), change `ask_resolved`. Both idempotent per id.
- `POST /v1/sandbox/heartbeat` every 15 s:
  ```json
  {"turnId": "…"|null, "state": "running"|"stopping"|"held"|"idle", "tasks": 2,
   "lastTurn": {"id": "…", "outcome": "completed"|"failed"|"canceled", "error": {"code", "message"}|null} | null,
   "gen": 3, "supervisor": {"version": "supervisor-1a2b3c4d5e6f", "bootId": "…", "restarts": 0},
   "cc": {"quotaExceeded": false, "blockedWrites": 0, "oldestBlockedMs": 0}}
  ```
  Sets `heartbeat_at` and pushes a running turn's deadline forward (8.8). Reconciliation, in the heartbeat's
  transaction (the backstop for a report lost against all odds):
  1. magpie's active turn `T` is running or stopping, and `lastTurn.id == T` with `turnId != T`: end `T` with
     `lastTurn`'s outcome and error, exactly as if the end report had arrived (the `lastUuid` check is skipped: the
     supervisor only reports `lastTurn` after its writes landed).
  2. magpie's active turn `T` is running or stopping, its `start_turn` was acked, and the heartbeat shows `idle` with
     `turnId` null and `lastTurn.id != T`: fail `T` with `run.error` ("the sandbox no longer knows this turn").
  3. `turnId` names a turn magpie does not know and magpie has no active turn: open it as an agent-started turn (as
     `POST /v1/sandbox/turns`).
  4. No active turn: `state: held` sets `run_state = 'held'`; `state: idle` while magpie says `held` sets `idle` and
     clears `background`.
  The extra fields (`gen`, `supervisor`, `cc`) are logged and exposed to admin reads.
- Title result: the `title` command's ack `{"result": {"response": <Messages API response JSON>}}` or
  `{"result": {"error": "…"}}` (section 9.4).

### 8.6 Cancel (M8)

`POST /v1/conversations/{id}/cancel` `{}` (pigeon; allowed while viewing as, recorded as `cancel` in `view_as_log` in
the same transaction) → 202 `{"turnId": "<the turn being stopped>"|null}`:
- Active turn `admitted`/`preparing`, or `starting` with the `start_turn` command not yet delivered: cancel its
  commands, release the driver claim, `turn.ended` canceled at once. The user's message stays (sent, unanswered).
- Active turn delivered (`starting`, `running`): `turn.stop_requested`, state `stopping`, command `interrupt`. The
  supervisor reports `canceled` (or a normal end if the turn finished first). After 30 s without a report: command
  `kill` `{reason: "stop"}`, `turn.ended` canceled with error `stop.forced`, `run_state = 'idle'`. Cancel never waits
  on a dead link (today it can hang for 15 min).
- Already `stopping`: nothing new (idempotent), 202 with the same turn id.
- Active turn `parked` (8.1a): its asks resolve canceled, `turn.ended` canceled at once.
- No active turn and `run_state = 'held'`: command `stop_tasks` (the supervisor stops every background task and acks
  `{"stopped": n}`; the next heartbeat or end report moves the conversation to `idle`), 202 `{"turnId": null}`.
- Nothing running: no-op, 202 `{"turnId": null}`. Never an error, so pigeon never shows a failure for a cancel
  (pigeon 7.5.6).

### 8.7 Sandbox events from hermit (the pull feed, R8)

magpie's job leader follows hermit's feed (hermit 3.6.5): `GET /v1/sandbox-events?after=<seq>&limit=500&waitMs=25000`
(35 s timeout), starting from `hermit_feed.after_seq` (`latest` on first start), storing the cursor after each batch
in the same transaction as the batch's effects. For each event:
- Update the conversation's `hermit_sandbox`, `hermit_state`, `hermit_version` when the event's `version` is newer
  (unknown conversations and migrating ones are ignored).
- `state: gone`: if `lease_sandbox` is that sandbox, clear the lease. A turn of that sandbox in `starting`, `running`
  or `stopping` ends (a `parked` turn has no sandbox and is left alone, 8.1a): `failed` with `sandbox.lost` (release
  codes `sandbox_lost`, `sandbox_released`), or `failed` with
  `sandbox.busy` / `sandbox.failed` for a turn still waiting (the driver normally sees these first through its
  long-poll; whichever commits first wins, the other finds the turn ended). A `held` conversation goes `idle`.
- `state: starting|ready`: recorded only (the driver's own long-poll drives the turn and the preparing chip).
- 410 `cursor_expired`: re-read `GET /v1/conversations/{id}/sandbox` for every conversation with an active turn or a
  lease, apply what it says, and continue from `latest`.

The feed is the one mechanism for "the sandbox died"; the driver's long-poll is the one mechanism for "the sandbox is
starting / ready"; the watchdog (8.8) asks hermit only when heartbeats stop. There is no fixed-interval poll.

hermit must not release a sandbox with an active turn except for deletion, offboarding, host loss or a drain
deadline; when it does, the supervisor ends the turn (`sandbox_released` path) or the `gone` event above does, within
seconds, instead of a spinner that never stops.

### 8.8 Deadlines and the watchdog

`turns.deadline` is defined per state and recomputed on every transition:

| State | Deadline | On expiry |
|---|---|---|
| `admitted`, `preparing` | `created_at + 15 min` (a resumed parked turn: `resumed_at + 15 min`; the re-acquire cap of 45 min counts from the same) | failed `sandbox.timeout` |
| `starting` | `ready_at + 120 s` | if `start_turn` is unacked: failed `sandbox.unreachable` + `kill {reason: watchdog}`; if acked (the `running` report is in flight): +30 s |
| `running` | `greatest(heartbeat_at, availability.available_since) + 90 s`, pushed forward by every heartbeat | the check below |
| `stopping` | `stop_requested_at + 30 s` | as 8.6 (`stop.forced`) |
| `parked` | none: the wait for the person counts in no turn timer (8.1a) | — |

A running turn has no total time limit in magpie: a long turn with heartbeats runs for as long as it runs. The
sandbox's lifetime is hermit's (8 h, waiting for a busy sandbox up to a 60 min grace, hermit 4.2); a turn cut by that
ends through 8.7 with `sandbox.lost`.

The running check, on expiry: magpie asks hermit `GET /v1/conversations/{id}/sandbox` (no wait). hermit's view comes
from the supervisor's own activity reports to hermit and hermit's direct health checks of :7070, which do not depend
on magpie.
- `gone`, `releasing`, a different sandbox id, or 404 `no_sandbox` → failed `sandbox.lost`.
- Ready and `lastActiveAt` within 90 s (alive per hermit) → deadline + 90 s; once the silence (`now() -
  greatest(heartbeat_at, available_since)`) exceeds 5 min while hermit still says alive → failed
  `sandbox.unreachable` + `kill {reason: watchdog}`.
- Ready and `lastActiveAt` older than 90 s → failed `sandbox.unreachable` + `kill {reason: watchdog}`.
- hermit unreachable → deadline + 90 s (no turn is failed on missing evidence).

Silence is measured only over time magpie could accept heartbeats: the `availability` row (section 4) is ticked by
every replica's sweeper every 5 s (`last_tick = now()`), and when a tick finds `last_tick` older than 20 s (no
replica could reach the database, or none ran), it sets `available_since = now()` first. After a magpie or database
outage of any length, a running turn therefore gets a full 90 s for its supervisor's heartbeat retry to land, and a
healthy turn whose writes were only waiting for magpie is never killed. The same rule makes late transcript lines
land before `turn.ended`, not after.

The sweeper runs every 5 s on every replica (`for update skip locked` over `turns_open` by `deadline`) and skips
migrating conversations and `import`-origin turns (cutover 6.4, 6.6).

### 8.9 Error codes

`turn.ended.error.code` (data, dotted; pigeon localises by code; magpie never produces sentences):

| Code | Meaning | Today's sentence (moves to pigeon) |
|---|---|---|
| `sandbox.busy` | hermit has no capacity, or sandboxes are disabled (`detail` = hermit's reason) | 所有電腦正在使用中… |
| `sandbox.not_allowed` | 403 `not_allowed` from hermit | notAllowed |
| `sandbox.failed` | the sandbox could not be started | 未能啟動這個對話的電腦… |
| `sandbox.timeout` | not ready within 15 min | (new) |
| `sandbox.unreachable` | started but the supervisor never took the turn, or went silent while hermit sees it | 暫時連接不到這個對話的電腦，請稍後再試。 |
| `sandbox.lost` | the sandbox died or was released mid-turn | (new: today a silent cut-off) |
| `run.exited` | Claude Code exited abnormally (`message` carries the exit code) | sandboxFailed (wrongly today) |
| `run.oom` | the supervisor's memory guard killed Claude Code | (new) |
| `run.error` | the supervisor reported an error with a message, or reconciliation found the turn unknown | — |
| `cc.write_stuck` | one transcript write kept failing for 10 min while magpie was reachable (6.12) | (new) |
| `stop.forced` | cancel not confirmed in 30 s | — |
| `legacy.cut` | an old-stack turn cut at the switch (cutover 6.6) | (cutover) |
| `internal` | a magpie bug | — |

Model errors are tags on the `cc.assistant` line Claude Code writes as `API Error: …`, in `meta.error`:
`{"code": "model.budget", "status": 402, "message": "<egress's own localised sentence>"}`, `model.rate_limited`
(429, rate_limit_error), `model.unavailable` (>= 500, 529, overloaded_error, api_error), `model.access` (401/403),
`model.refused` (other 4xx, with the provider's message), `model.connection` (no status). The rules are today's
`jarvis-errors.ts` regexes, moved into `classify.rs`; pigeon applies the same rules to the row text (pigeon 7.9).

---

## 9. Conversations

### 9.1 The Conversation object

pigeon 14.1.0's shape, plus magpie's additive fields (pigeon ignores unknown fields):

```json
{
  "id": "a103219b-…",
  "title": "港珠澳大橋今日新聞",
  "titleSource": "generated",
  "createdAt": "…", "updatedAt": "…", "lastUserMessageAt": "…",
  "deletedAt": null, "purgeAt": null,
  "claudeSessionId": "279aabf0-…",
  "turn": {"id": "…", "startedAt": "…", "origin": "person", "askWaitMs": 0},
  "lastTurn": {"id": "…", "outcome": "completed", "endedAt": "…", "error": null},
  "sandbox": "preparing" | "ready" | null,
  "asks": [{"id": "…", "toolName": "AskUserQuestion", "toolUseId": "…", "input": {}, "createdAt": "…"}],
  "migrating": false,
  "run": {"state": "running", "background": []},
  "model": "deepseek/deepseek-v4.1-flash-1017",
  "contextTokens": 51234,
  "origin": {"kind": "new"}
}
```

- `titleSource`: `name_source` null with a title → `"provisional"`, `'title'` → `"generated"`, `'user'` → `"user"`,
  no title → null. `title` null means untitled: pigeon shows its localised placeholder (「新對話」).
- `updatedAt` = `activity_at`. `purgeAt = deletedAt + 30 days`.
- `turn` is non-null while a turn is admitted, preparing, starting, running, stopping or parked (the app shows it
  running); `origin` is `person` or `agent`. `askWaitMs`: how long the turn's answered questions waited
  (`ask.pending` to `ask.resolved`), so working time is `now - startedAt - askWaitMs` while no question is open and
  stands still while one is (DECISIONS 17; pigeon 7.7). `lastTurn` is the most recent ended turn, with `error: ErrorInfo` when failed.
- `sandbox` = `sandbox_phase` (`preparing` while acquisition shows the chip, `ready` once ready during that turn,
  null otherwise).
- `asks`: the open ask (at most one today), else `[]`.
- `migrating`: the importer holds it (cutover 6); every person mutation answers 409 `conversation_migrating`.

### 9.2 Create (M3) and creation lookup (M4)

`POST /v1/conversations`
```json
{"idempotencyKey": "draft-7f3a", "message": {"clientMessageId": "draft-7f3a:initial-message",
 "text": "…", "images": [{"uploadId": "…"}], "files": [{"path": "uploads/…/…", "name", "mimeType", "size"}]}}
```
- `(person, idempotencyKey)` is unique: a repeat (another socket, a reconnect, another pigeon replica) returns the same
  conversation and turn (200, same body). Today the key lives per socket, so a retry after a reconnect makes a second
  conversation.
- The id is a new UUIDv4. Provisional title: the first 24 characters (Unicode scalar values) of the text, plus `…`
  when longer; null when the text is empty. `name_source` null.
- Appends `conversation.created`, then admits `message` exactly as 8.2 when its text or attachments are non-empty.
  While viewing as: allowed, `viewer` set, `view_as_log` `create` row in the same transaction.
- 201 `{"conversation": Conversation, "turn": {"id", "startedAt"} | null}`; 400 `bad_request`, 413, 415 as uploads.

`GET /v1/creations/{idempotencyKey}` (M4, the app's `creation.subscribe` after a reconnect mid-create) → 200
`{"conversation", "turn"}` for the person's create with that key, else 404 `not_found` (the app then re-sends the
create with the same key, which magpie de-duplicates).

### 9.3 List and details (M1, M2)

- `GET /v1/conversations?state=active|deleted&limit=200&cursor=<opaque>&search=<title substring>`
  → `{"conversations": [Conversation], "nextCursor": "…"|null, "list": {"generation": "<change_generation>",
  "headSeq": <change_seq>}}`. Active: by `activity_at desc, id desc`; deleted: by `deleted_at desc`. `limit` 1-200
  (the app's page is 200). `search` (deleted only) is a case-insensitive title substring (Recently deleted search).
  `list.headSeq` is read in the same snapshot, so pigeon can tail the change feed from it without a gap. A cursor that
  does not parse: 400 `bad_cursor`.
- `GET /v1/conversations/{id}` → Conversation, deleted ones included.

### 9.4 Titles

Precedence: a rename (`name_source 'user'`) always wins; a title-model title (`'title'`) replaces a provisional
name; Claude Code's own title records only replace a provisional name.

The title-model call runs in the conversation's sandbox, executed by the supervisor through the gateway's `model`
endpoint, so zero data retention, the model list, the budget and cost attribution under that sandbox apply as today.
There is no per-person process any more and magpie never calls the model itself. magpie:
1. At admission, when `name_source` is null and `title_attempts < 3`, queues command `title` with
   `{"userPrompt": "Here is the first message of the conversation:\n\n<message>\n<first message, ≤ 2,000
   chars>\n</message>\n\nReply with only the title.", "maxTokens": 100}`. The supervisor adds the `system` text
   (the title instructions are instance content and come from hermit, today's `TITLE_SYSTEM` moved verbatim) and its
   small/fast model, and acks with the response body.
2. On the ack: take the last text block (thinking ignored) and clean it exactly as today's `cleanTitle`: first
   non-empty line; strip a leading `title:`/`標題:`/`标题:` label; strip trailing `.`/`。`; strip surrounding quotes
   and brackets `"'“”‘’「」『』《》*#` and spaces; collapse spaces; at most 80 characters.
3. `update conversations set title = $t, name_source = 'title' where id = $id and name_source is null`; if a row
   changed, append `title.changed` and a `conversation_upserted` change. A rename in between wins and nothing is
   announced.
4. An error or an empty title increments `title_attempts`; the next turn tries again (at most 3 attempts, where today
   a failure leaves the provisional name for good).

### 9.5 Rename (M5)

`PATCH /v1/conversations/{id}` `{"title": "…"}` → trimmed, 1-200 characters (400 `empty_title` /
`title_too_long`), sets `name_source = 'user'`, appends `title.changed`, change `conversation_upserted`; 200
Conversation. 403 `view_as_read_only` while viewing as; 409 `conversation_migrating`.

### 9.6 Delete, restore, purge (M6)

- `POST /v1/conversations/{id}/delete` (soft): `deleted_at = now()`, change `conversation_upserted` (with
  `deletedAt`), outbox `release_sandbox {reason: "deleted"}` (hermit 3.6.4: a deleted conversation has nothing to keep
  running; a running turn ends through the release, the supervisor reporting it, or 8.7). Events, rows, blobs and the
  workspace stay; share links of the conversation answer 404 while it is deleted (12.3). 200 `{"deletedAt": "…"}`.
  Idempotent.
- `POST /v1/conversations/{id}/restore`: clears `deleted_at`, change `conversation_upserted` (`deletedAt: null`);
  shares work again; the next turn gets a new sandbox. 200 Conversation.
- `DELETE /v1/conversations/{id}`: permanent; only for a conversation already in Recently deleted (409 `not_deleted`
  otherwise; the app's "delete for good" only exists there). Runs the purge below at once. 204.
- All three answer 403 `view_as_read_only` while viewing as, and 409 `conversation_migrating` while migrating.

Purge (permanent delete), one transaction per conversation:
1. Revoke its live shares (`revoked_at`, reason `deleted`), delete their snapshot scopes (cc rows, `share_rows` and
   blob refs), and queue `delete_workspace` for each share's `workspace_id`.
2. Delete the conversation row (cascades: events, rows, row_seqs, turns, commands, tool_links, uploads, sent_files,
   sent_staging, report_ids), its cc scope rows, its blob refs, its live frames.
3. `persons.purged_conversations + 1`; change `conversation_removed {reason: "purged"}`; NOTIFY (held polls of the
   conversation answer 404).
4. Queue `release_sandbox` (reason `deleted`) and `delete_workspace` (the conversation's workspace) in the outbox.
Unlike today, the whole session folder, tool results, subagent transcripts, uploads and sent files go with it, and a
failed hermit call is retried from the outbox instead of leaving an orphan for ever. The blob GC frees the bytes 24 h
later.

The 30-day purge is one job for the whole deployment (section 15), not a per-person process to wake.

Person purge (offboarding): `POST /v1/hermit/persons/{person}/purge` `{"reason": "offboarded"}` (caller hermit only).
It is called by hermit's operator route `POST /v1/admin/people/{email}/purge` (hermit spec, recorded in hermit's
`admin_events`), which hermit allows only for a person who is not in the catalogue; no automatic job deletes a
leaver's data (decision 7: manual operator purge only). magpie re-checks `GET /v1/people/{email}` at hermit and answers 409 `person_allowed` if hermit still
allows the person. Otherwise it purges every conversation of the person as above (shares revoked with reason
`offboarded`), 202 `{"conversations": n}`; idempotent.

### 9.7 Workspace references for hermit's cleanup

`POST /v1/hermit/workspaces/exists` `{"ids": ["<uuid>", …]}` (caller hermit, at most 1,000 ids) → 200 `{"exists":
["<uuid>", …]}`: the subset that magpie still references: conversation ids of every non-purged conversation (deleted
ones included) and `workspace_id` of every live share. hermit's `legacy-cleanup` uses it before removing a `ws/<id>`
folder (hermit 22.3 step 5 calls this route instead of a `GET /conversations/exists`).

---

## 10. Uploads and attachments

### 10.1 Upload (M13)

`POST /v1/uploads` with the raw body (chunked), `Content-Type`, `Aviary-Upload-Name: <percent-encoded file name>` and
`Aviary-Upload-Kind: file|image` (pigeon streams the app's multipart upload or the inline base64 image it received
over the socket). Limits: 50 MiB per file (52,428,800 bytes, the app's `FILE_UPLOAD_MAX_BYTES`; one number
everywhere), 413 `too_large` otherwise. `Kind: image` whose bytes are not a supported image (10.2): 415
`unsupported_type`. Allowed while viewing as (recorded as `upload` in `view_as_log`).

Response 201 (pigeon 14.1 M13, plus image facts):
```json
{"uploadId": "0b4c…", "path": "uploads/0b4c…/IMG_0042.HEIC", "name": "IMG_0042.HEIC", "size": 2211234,
 "mimeType": "image/heic", "width": 4032, "height": 3024}
```

`path` is an opaque reference the app sends back in the next turn's `files[]` (8.2 step 4); the final workspace path
(`uploads/<name>`, de-duplicated) is assigned at admission and shown in the user row. The body is hashed while it
streams to the bucket; the blob row, the upload row and an `upload` blob ref commit together. Unattached uploads older
than 24 h are deleted by the retention job (today's staging folder is never cleaned).

### 10.2 Images

An image is any of JPEG, PNG, GIF, WebP, HEIC/HEIF, AVIF, TIFF or BMP (by sniffed type, then extension); SVG is a
file. At upload magpie makes the model copy exactly as today: if the image is a model-native type, long edge <=
2,000 px, <= 3,932,160 bytes and has no EXIF rotation, the original is used; otherwise decode (HEIC via libheif),
apply EXIF orientation, fit to 2,000 px, keep PNG for transparent PNG/WebP when it fits, else JPEG at quality 85, 70,
then 55 on white. A `Kind: file` upload that happens to be an image stays a file. Limits per turn: 10 images and 10
files. The model copy is also the `display` variant (5.4).

### 10.3 In the conversation

At admission the uploads are attached (8.2 step 4) and appear in `user.sent` (`files`, `images`) and in the user row.
The supervisor writes the originals into `/workspace/uploads/<name>` and sends the model copies as image blocks
(supervisor 12.1). magpie never writes the workspace, so workspace quotas and ownership stay hermit's and the
supervisor's (today's missing-quota hole for conversations whose first message had an attachment cannot happen:
hermit ensures the quota at every start).

---

## 11. Files panel and sent-file snapshots

### 11.1 The panel

`GET /v1/conversations/{id}/files` →
```json
{"files": [{"path": "report/q3.xlsx", "name": "q3.xlsx", "size": 48211, "mimeType": "…", "isImage": false,
            "sentAt": "…", "caption": "…", "toolUseId": "call_9f…", "blobId": "…"}]}
```
Newest send first; a path sent more than once appears once, with its latest send. Built from `sent_files`, not by
reading the transcript.

### 11.2 Taking a snapshot (from the sandbox)

The SendUserFile PreToolUse hook asks the supervisor locally; the supervisor reads the files from `/workspace`
(refusing symlinks and anything outside it) and calls magpie through the gateway:

1. `POST /v1/sandbox/sent-files` (lease)
   `{"toolUseId": "call_9f…", "caption": "…", "files": [{"path": "/workspace/report/q3.xlsx", "size": 48211,
   "sha256": "<hex>", "mimeType": "…"}]}`
   Validates: `toolUseId` `^[A-Za-z0-9_-]{1,128}$`; 1-100 files; each path absolute under `/workspace` (a
   `file://` prefix is stripped), normalised, no `..`, no NUL, <= 4,096 bytes; each file <= 64 MiB (the app's
   download cap); total <= 256 MiB. Stores `sent_staging`. Answers `{"missing": ["<sha>", …]}`: the blobs magpie
   doesn't have yet.
2. `PUT /v1/sandbox/blobs/{sha}` for each missing one (raw body streamed to the bucket, verified against the sha, 400
   `sha_mismatch`).
3. `POST /v1/sandbox/sent-files/{toolUseId}/commit` → one transaction: `sent_files` rows, conversation blob refs, a
   `files.sent` event (with `toolUseSeq` from `tool_links` when the assistant line is already stored), the tool row
   update (7.3 rule 6), drop the staging row. 200 `{"kept": n, "seq": 650}`.

Every step is idempotent per `toolUseId`. The supervisor's hook blocks the tool (exit 2) on any non-200 so the model
is told the files were not sent, as today. The egress `sent-files` route and `JARVIS_SENT_FILES_TOKEN` disappear;
the snapshot is magpie's own data in its bucket, deployment-independent, deduplicated by content, and copied for free
on share and continue.

### 11.3 Downloads (M14)

`GET /v1/conversations/{id}/files/raw?path=<rel>&sent=<toolUseId>&inline=1` (one `Range` honoured)
- With `sent`: the bytes of that send's snapshot (`sent_files` blob). 404 `not_found` if the path was not in that
  send.
- Without `sent`: the live workspace file, streamed from hermit `GET /v1/workspaces/{id}/files?path=<rel>` (hermit
  3.8: `Range`, `etag`, `last-modified` passed through; hermit enforces inside-root, no-magic-link opening). hermit's
  404 `no_path` and 409 `not_a_file` → 404 `not_found`; 503 `workspace_unavailable` → 503 `unavailable`. This serves
  "paths named in replies" and markdown images that name workspace files (which today show "unavailable").
- `path` must be relative, normalised, without `..` or NUL, <= 4,096 bytes (400 `bad_path`).
- Headers as today: `content-type` from the extension table; inline only for `image/*` except SVG, and PDF, when
  `inline=1`; otherwise `content-disposition: attachment; filename*=UTF-8''…`; `x-content-type-options: nosniff`;
  `content-security-policy: default-src 'none'; sandbox`; `cache-control: private, no-cache`; `accept-ranges`,
  `content-range`, `etag` as the source gives them.

---

## 12. Shares

A share is a frozen copy: its own timeline rows, its own Claude Code scope, and its own workspace, which is a
copy-on-write clone in hermit with the id `workspace_id = uuidv5(SHARE_NS, "share:" + shareId)`, `SHARE_NS =
6f1c9d2e-0b7a-4f53-9a43-5b2f3f1e8c61` (the same convention hermit and the cutover use for imported shares). Shares use
only hermit's workspace calls (`clone`, `files`, `DELETE`); hermit's `snapshot` route (501 in YYC) is not used
(platform R28).

### 12.1 Create (freeze)

`POST /v1/conversations/{id}/shares` → 201 `{"id": "<22 chars>", "createdAt": "…"}` (pigeon builds `/s/<id>`).
- id: 16 random bytes, base64url, 22 characters (today's format; imported links keep working).
- `upto_seq`: if a turn is active, the event seq just before its `user.sent` (only finished turns are shared);
  otherwise `head_seq`. 409 `nothing_to_share` if no assistant row exists whose events are all `<= upto_seq`.
- First, hermit `POST /v1/workspaces/{workspace_id}/clone` `{"from": "<conversation id>"}` (a metadata-only clone in
  YYC). If it fails, the answer is 500 `share_failed` and nothing is stored.
- Then one transaction: the share row (owner, `owner_name` = the email's local part, title = the conversation's
  title); `share_rows` = the conversation's rows whose source events are all `<= upto_seq`, as they are now (rows of a
  finished turn do not change later except by a retract, which then does not reach the share); a frozen copy of the
  conversation's cc scope into a new snapshot scope (row copies only, cut at the first line of the active turn by
  `ord`; blobs are shared by reference, so this is fast at any transcript size); `share` blob refs for every blob the
  frozen lines, the frozen rows and the events `<= upto_seq` reference. If the transaction fails, `delete_workspace`
  for the clone goes to the outbox.
- Every call makes a new link (as today). 403 `view_as_read_only` while viewing as; 409 `conversation_migrating`.

Today a share takes 6.4 s because it copies the workspace and the transcript; here the rows and the Claude Code state
are row copies and the workspace a clone, so the target is under 1 s plus hermit's clone.

### 12.2 List and revoke (owner)

- `GET /v1/conversations/{id}/shares` → `{"shares": [{"id", "createdAt"}]}`, live links, newest first.
- `DELETE /v1/shares/{shareId}` → 204 for the owner's live link; anyone else's, or an unknown or revoked id, 404
  `not_found`. Revoke sets `revoked_at` (reason `owner`), deletes the snapshot scope, `share_rows` and share blob refs,
  and queues `delete_workspace` for `workspace_id`. Revoked rows are kept (audit), without their data.

### 12.3 View (any signed-in person)

- `GET /v1/shares/{shareId}` → `{"id", "title", "ownerName", "isOwner", "createdAt", "rows": [Row]}`: every frozen
  row, ascending by `seqStart` (pigeon projects them with the live projection, pigeon 5.10). The response is streamed
  as it is serialised (the largest conversation today gives a few MB of rows; blobs stay references).
- `GET /v1/shares/{shareId}/files/raw?path&sent&inline`: with `sent`, sent files of `files.sent` events `<= upto_seq`;
  without `sent`, attachments of `user.sent` events `<= upto_seq` by their `uploads/<name>` path, else the file from
  the share's frozen workspace (`GET /v1/workspaces/{workspace_id}/files?path=` at hermit; an imported share whose
  clone hermit has not made yet answers 404 until it exists, cutover 7.4.4). Never the live workspace.
- Images of shared rows load through `GET /v1/blobs/{blobId}` (pigeon signs them; the share's blob refs keep them
  alive after the source is purged).
- A revoked share, a share whose source is soft-deleted, or one whose source was purged answers 404 `not_found`
  everywhere. (Decision: a link of a conversation in Recently deleted stops working and comes back on restore. Today's
  code keeps it working, which contradicts its own stated intent.)
- The audience is everyone pigeon signs in; the 128-bit id is the capability.

### 12.4 Continue (「繼續對話」)

`POST /v1/shares/{shareId}/continue` → 201 `{"conversationId": "…"}` (403 `view_as_read_only` while viewing as):
1. Mint the new conversation id; hermit `POST /v1/workspaces/{new id}/clone` `{"from": "<workspace_id>"}`. On
   failure: 500 `continue_failed`, nothing stored.
2. One transaction: a new conversation for the viewer (title = the share's title, `name_source 'title'`, `origin:
   {"kind": "continue", "shareId"}`, model and provider session id from the share), events `1..upto_seq` copied with
   their seqs (so `head_seq = upto_seq`), `share_rows` copied as its rows with their seqs (`row_head` = the largest
   `seq_end`; `row_seqs` rebuilt from them), the share snapshot's cc scope copied into the new conversation's scope,
   conversation blob refs, `sent_files` and `tool_links` rows `<= upto_seq`, change `conversation_upserted`. If it
   fails, `delete_workspace` for the clone goes to the outbox.
3. The sandbox is not carried over; the first turn gets a new one.

Claude Code session ids and message uuids are kept as they are: each conversation has its own file namespace, so
nothing collides and no rewriting is needed (today's rewrite exists only because all of a person's conversations
shared one folder). The copy is independent: a later revoke or purge of the source does not touch it.

---

## 13. View-as, audit and admin reads

- pigeon starts view-as by recording it: `POST /v1/view-as` `{"admin", "person", "session", "expiresAt"}` (M17) → 201
  `{}`. pigeon must not open the view without the 201.
- While `Aviary-Viewer` is present, every allowed state change is recorded in the same transaction as the change, so
  "record, then act" can never half-fail (pigeon 14.1 relies on it):
  - create (M3): `view_as_log` `create` with the conversation; send (M7): `send` with the conversation, the
    `user.sent` seq and the message length only (never contents), and `events.viewer` set; cancel (M8): `cancel`;
    upload (M13): `upload` (size only).
  - reads are allowed and not recorded per call (the `start` row covers the session);
  - everything else that changes state answers 403 `view_as_read_only`: rename, delete, restore, permanent delete,
    share create, revoke, continue, ask answer.
- `GET /v1/ops/view-as?limit=100&before=<id>` (hermit, admin) → `{"viewAs": [{"at", "conversationId", "event",
  "admin", "user", "session", "chars"}]}` newest first (platform 15.3; `event` is `start`, `create`, `send`,
  `cancel`, `upload` or `admin_read`). Kept for good; today's 39 `ops_events` rows are imported.
- Admin reads without pigeon (debugging a chat without SQL), caller admin with `Aviary-Admin: <email>`:
  `GET /v1/admin/conversations?person=&state=`, `GET /v1/admin/conversations/{id}` (the Conversation plus `lease`,
  `heartbeat`, the last heartbeat's `gen`/`supervisor`/`cc` fields, `hermitSandbox`), `/{id}/events` (7.2 events page shape)
  and `/{id}/timeline` (M9 shape). Each first read of a conversation by an admin per day writes a `view_as_log` row
  `admin_read`.

---

## 14. Live fan-out across replicas

- Each replica holds one dedicated Postgres connection (outside the pool) that `LISTEN`s on four channels:
  - `magpie_rows`, payload `<conversation uuid>:<row_head>`, sent by every transaction that writes rows or bumps the
    epoch;
  - `magpie_changes`, payload `<person_key>:<change_seq>`, sent by every change-feed insert;
  - `magpie_commands`, payload `<conversation uuid>:<cmd_head>`, sent when a command is queued or canceled;
  - `magpie_live`, payload `<conversation uuid>:<n>`, sent by live-frame inserts.
- In memory: `HashMap<key, tokio::sync::watch::Sender<u64>>` per channel kind, created on first waiter and dropped
  with the last. A notification bumps the watch; held long-polls wake and re-query Postgres. The rows are the truth;
  notifications only say "look now", so a duplicated or coalesced notification is harmless.
- If the LISTEN connection drops: reconnect with backoff (0.5 s doubling to 5 s), then wake every waiter (they
  re-query), so nothing missed in the gap is lost. Every held poll also re-queries every 1 s regardless.
- NOTIFY is never exposed outside magpie; pigeon and the supervisor only see long-polls.
- Payloads are under 100 bytes (the 8,000-byte NOTIFY limit that made today's relay skip large frames is irrelevant).

---

## 15. Background jobs

One replica at a time holds the job leader lock: `pg_try_advisory_lock(hashtext('magpie.jobs'))` on a dedicated
connection, retried every 10 s by the others. Losing the connection releases it; a draining replica releases it
first. Every job is idempotent, so a takeover mid-pass is safe.

| Job | Who | Every | What |
|---|---|---|---|
| Purge | leader | 10 min | conversations with `deleted_at <= now() - 30 days`, 20 per pass, each purged as 9.6 |
| Outbox | leader | 5 s | hermit calls due (`next_at`): `release_sandbox` → `POST /v1/conversations/{id}/sandbox/release {reason}`, `delete_workspace` → `DELETE /v1/workspaces/{wid}`; backoff 5 s doubling to 1 h; an item failing for 24 h logs an error every hour; 404 from hermit counts as done |
| hermit feed | leader | continuous long-poll | 8.7 |
| Blob GC | leader | 1 h | unreferenced stored blobs older than 24 h (5.3); weekly bucket reconcile |
| Retention | leader | 1 h | unattached uploads > 24 h; acked commands > 24 h; `cc_ops`, `report_ids` > 24 h; `changes` > 30 days; staging rows (`sent_staging`) > 1 h; `moved` limbo > 10 min |
| Blob uploader | every replica, skip locked | 1 s | `blob_staging` → bucket (5.2) |
| Availability tick | every replica | 5 s | 8.8 |
| Limbo flush | every replica, skip locked | 5 s | `truncated` limbo of files quiet for 10 s → `retract` events (6.7) |
| Live cleanup | every replica, skip locked | 10 s | live frames of turns ended > 60 s ago (7.5) |
| Turn sweeper | every replica, skip locked | 5 s | lapsed driver claims, turn deadlines (8.3, 8.8) |

Jobs skip migrating conversations (cutover 6.4).

---

## 16. REST reference

All bodies are JSON (camelCase, platform 3.6) unless noted. Errors are `{"code": "<lower_snake_case>", "message":
"<English, for logs>", …}` with the status shown; pigeon maps codes to localised text. `{id}` is a conversation uuid
unless stated. Every route answers 401 `unauthenticated` / 403 `wrong_caller` per section 3, and 503 `unavailable`
(with `retry-after`) when Postgres cannot be reached. Every mutation of a migrating conversation answers 409
`conversation_migrating` `{retryAfterMs}` (cutover 6.4). The M-numbers are pigeon 14.1's.

### 16.1 pigeon (`Aviary-Person` required except M15 and M17; `Aviary-Viewer`, `Aviary-View-Session` optional)

| # | Method and path | Request | Response | Errors |
|---|---|---|---|---|
| M1 | `GET /v1/conversations` | `state=active\|deleted, limit, cursor, search` | `{conversations, nextCursor, list: {generation, headSeq}}` | 400 `bad_cursor` |
| M2 | `GET /v1/conversations/{id}` | | Conversation | 404 |
| M3 | `POST /v1/conversations` | `{idempotencyKey, message: {clientMessageId, text, images: [{uploadId}], files: [{path, name, mimeType, size}]}}` | 201 `{conversation, turn}`; 200 repeat | 400 `bad_request` / `unknown_upload` / `too_many_attachments`; 413 |
| M4 | `GET /v1/creations/{idempotencyKey}` | | `{conversation, turn}` | 404 |
| M5 | `PATCH /v1/conversations/{id}` | `{title}` | Conversation | 400 `empty_title` / `title_too_long`; 403 `view_as_read_only`; 404 |
| M6 | `POST /v1/conversations/{id}/delete` | | `{deletedAt}` | 403 `view_as_read_only`; 404 |
| M6 | `POST /v1/conversations/{id}/restore` | | Conversation | 403; 404 |
| M6 | `DELETE /v1/conversations/{id}` | | 204 | 409 `not_deleted`; 403; 404 |
| M7 | `POST /v1/conversations/{id}/turns` | `{clientMessageId, text, images, files}` | 202 `{turn: {id, startedAt}}`; 200 repeat | 409 `turn_running` / `conversation_deleted`; 400; 413; 404 |
| M8 | `POST /v1/conversations/{id}/cancel` | `{}` | 202 `{turnId \| null}` | 404 |
| M9 | `GET /v1/conversations/{id}/timeline` | `tail=1 \| before=S \| after=S; limit; waitMs (after only); liveAfter` | `{epoch, minSeq, maxSeq, hasOlder, hasNewer, rows, live}` | 404 |
| — | `GET /v1/conversations/{id}/live` | | LiveState; 204 when no turn is active | 404 |
| M10 | `GET /v1/conversations/{id}/prompts` | | `{epoch, prompts: [{seq, timestamp, preview}]}` | 404 |
| — | `GET /v1/conversations/{id}/search` | `q, cursor, limit` | `{epoch, locations, nextCursor}` | 404 |
| M11 | `POST /v1/conversations/{id}/asks/{askId}/answer` | Paseo `AgentPermissionResponse` as is | 200 `{}` (queues `answer_ask`; for a parked turn's ask, resolves it and resumes the turn when it was the last, 8.1a) | 409 `already_resolved`; 403 `view_as_read_only`; 404 |
| M12 | `GET /v1/changes` | `after (int \| latest), limit, waitMs` | `{generation, headSeq, changes}` | 410 `cursor_expired` |
| M13 | `POST /v1/uploads` | raw body; `Content-Type`, `Aviary-Upload-Name`, `Aviary-Upload-Kind` | 201 `{uploadId, path, name, size, mimeType, width?, height?}` | 413 `too_large`; 415 `unsupported_type` |
| M14 | `GET /v1/conversations/{id}/files/raw` | `path, sent?, inline?`; `Range` | 200/206 bytes | 400 `bad_path`; 404 |
| — | `GET /v1/conversations/{id}/files` | | `{files}` | 404 |
| M15 | `GET /v1/blobs/{blobId}` | `variant=thumb\|display\|full`; no person header | bytes, `content-type`, `etag` | 404; 415 |
| M16 | `GET /v1/conversations/{id}/shares` | | `{shares: [{id, createdAt}]}` | 404 |
| M16 | `POST /v1/conversations/{id}/shares` | | 201 `{id, createdAt}` | 409 `nothing_to_share`; 403; 500 `share_failed` |
| M16 | `DELETE /v1/shares/{shareId}` | | 204 | 404 `not_found` |
| M16 | `GET /v1/shares/{shareId}` | | `{id, title, ownerName, isOwner, createdAt, rows}` | 404 `not_found` |
| M16 | `GET /v1/shares/{shareId}/files/raw` | `path, sent?, inline?` | bytes | 400; 404 |
| M16 | `POST /v1/shares/{shareId}/continue` | | 201 `{conversationId}` | 403; 404; 500 `continue_failed` |
| M17 | `POST /v1/view-as` | `{admin, person, session, expiresAt}` | 201 `{}` | 400 |

### 16.2 supervisor through the gateway (`Aviary-Sandbox-*` set by the gateway; `Aviary-Lease` on state changes)

| Method and path | Request | Response |
|---|---|---|
| `POST /v1/sandbox/lease` | `{}` | `{leaseId, epoch, conversationId, providerSessionId, mainTranscript, resync}`; 409 `not_current_sandbox` / `conversation_migrating`; 503 |
| `GET /v1/sandbox/cc/tree` | | `{files: [{path, kind, size, mtimeNs, mode, version}], ccBytes, quotaBytes}` |
| `GET /v1/sandbox/cc/file` | `path, offset?, length?` | bytes (206 with a range) |
| `POST /v1/sandbox/cc/write` | query `path, offset, mtimeNs, opId, create=0\|1, mode, turnId?`; raw body <= 16 MiB | `{size, version}`; 409 `offset_mismatch` / `offset_beyond_eof` / `lease_lost` |
| `POST /v1/sandbox/cc/truncate` | `{path, size, mtimeNs, opId}` | `{size, version}` |
| `POST /v1/sandbox/cc/rename` | `{from, to, opId}` (replaces `to`) | `{version}` |
| `POST /v1/sandbox/cc/unlink` | `{path, opId}` | 204; 403 `protected` |
| `POST /v1/sandbox/cc/mkdir` | `{path, mode, mtimeNs, opId}` | 204; 409 `exists` |
| `POST /v1/sandbox/cc/rmdir` | `{path, opId}` | 204; 409 `not_empty` |
| `POST /v1/sandbox/cc/setattr` | `{path, mode?, mtimeNs?, opId}` | `{version}` |
| `GET /v1/sandbox/commands` | `after, waitMs` | `{commands: [{cmdSeq, type, turnId, payload}], head}` |
| `POST /v1/sandbox/commands/{cmdSeq}/ack` | `{result}` | 204 |
| `POST /v1/sandbox/turns` | `{turnId, origin: "agent", at}` | 201 (200 repeat) |
| `POST /v1/sandbox/turns/{turnId}/state` | 8.5 | 204; 409 `bad_transition` / `transcript_pending` |
| `POST /v1/sandbox/turns/{turnId}/live` | `{frames}` (7.5) | 204; 409 `turn_not_active` |
| `POST /v1/sandbox/tasks` | `{events: [{id, …}]}` | 204 |
| `POST /v1/sandbox/asks` | `{askId, toolName, toolUseId?, input}` | 201 |
| `POST /v1/sandbox/asks/{askId}/resolved` | `{outcome: "answered" \| "canceled"}` | 204 |
| `POST /v1/sandbox/heartbeat` | 8.5 | 204 |
| `GET /v1/sandbox/blobs/{sha}` | `as=raw\|media\|text` | bytes |
| `PUT /v1/sandbox/blobs/{sha}` | raw body | 201/200; 400 `sha_mismatch` |
| `POST /v1/sandbox/sent-files` | 11.2 | `{missing}` |
| `POST /v1/sandbox/sent-files/{toolUseId}/commit` | | `{kept, seq}` |

Command types: `start_turn`, `interrupt`, `kill` (`{reason: "stop"|"watchdog"}`), `title`, `answer_ask`,
`stop_tasks`. Turn error codes a report may carry: `run.exited`, `run.error`, `run.oom`, `cc.write_stuck`.

### 16.3 hermit

| Method and path | Request | Response |
|---|---|---|
| `POST /v1/hermit/persons/{person}/purge` | `{"reason": "offboarded"}` | 202 `{conversations: n}`; 409 `person_allowed` (9.6) |
| `POST /v1/hermit/workspaces/exists` | `{ids: [uuid]}` (<= 1,000) | `{exists: [uuid]}` (9.7) |

hermit also reads the ops routes (16.4). magpie has no push route for sandbox events (R8: magpie pulls hermit's feed).

### 16.4 ops (callers hermit and admin) and admin (caller admin)

| Method and path | Response |
|---|---|
| `GET /v1/ops/people?tz=<IANA zone>` | `{"tz", "people": [{"owner", "total", "today", "last7d", "live", "deleted", "purged", "lastActiveAt"}]}` (platform 15.3); `today` from the zone's midnight; `live` = run_state not idle; `deleted` = in Recently deleted. An invalid zone falls back to UTC. |
| `GET /v1/ops/view-as?limit=&before=` | 13 |
| `GET /v1/ops/stats` | `{conversations, deleted, events, rows, ccBytes, blobs, blobBytes, stagedBlobs, stagedBytes, outboxPending, oldestOutboxAt, oldestDuePurgeAt, hermitFeedLagSeconds}` |
| `GET /v1/admin/conversations` (+ `/{id}`, `/{id}/events`, `/{id}/timeline`) | 13 (`Aviary-Admin` required) |

### 16.5 Health (port 9090, never in the Service, never through any gateway or tunnel)

`GET /healthz`, `GET /readyz`, `GET /version`, `GET /metrics` exactly as platform 3.2. Readiness is 503 only before
magpie has applied or verified migrations at this build's version and established its first LISTEN connection, and
while draining; it does not turn 503 when Postgres is unreachable later (requests then answer 503 `unavailable`).
Metrics: request latencies by route, cc write latency (by size class), staged blob count and bytes, blob upload
latency and failures, held long-polls, LISTEN reconnects, outbox depth, hermit feed lag, job runs, watchdog actions by
code, lease refusals.

---

## 17. Limits

| Limit | Value | Answer when hit |
|---|---|---|
| Upload file | 50 MiB (52,428,800 bytes) | 413 `too_large` |
| Attachments per turn | 10 images + 10 files | 400 `too_many_attachments` |
| Turn text | 1 MiB UTF-8 | 413 |
| Model image copy | long edge 2,000 px, <= 3,932,160 bytes | made to fit |
| Thumbnail / display copy | long edge 512 px / 2,000 px | — |
| Image decode | 40 megapixels | 415 |
| Title | 80 characters (model), 200 (rename) | cut / 400 |
| Provisional title | 24 characters + `…` | — |
| Title attempts | 3 per conversation | — |
| Timeline page | 1-500, default 40, 4 MiB JSON | `hasOlder/hasNewer` |
| List page | 1-200, default 200 | `nextCursor` |
| Long-polls (timeline, changes, commands) | `waitMs` <= 25,000; re-query every 1 s while held | empty answer |
| cc write body | 16 MiB per call | 413 (the supervisor splits larger writes) |
| JSONL line (incl. a tail being built) | 64 MiB | 413 `too_large` → EFBIG |
| Byte file | 256 MiB | 413 → EFBIG |
| Claude Code state per conversation | 2 GiB (`cc_bytes`) | 507 `quota_exceeded` → EDQUOT |
| Extraction thresholds | media >= 1 KiB base64; text >= 16 KiB (not assistant prose) | — |
| Text preview / row result preview | 4,000 characters | `truncated: true` |
| Blob staging total | 2 GiB | 503 for cc writes that add blobs |
| Sent files per send / per file / per send total | 100 / 64 MiB / 256 MiB | 400 / 413 |
| Paths | 4,096 bytes | 400 `bad_path` |
| Admission deadline (sandbox acquisition) | 15 min | `sandbox.timeout` |
| Preparing shown after | 1 s | — |
| Supervisor takes the turn after ready | 120 s | `sandbox.unreachable` |
| Heartbeat | every 15 s; deadline 90 s after the last (or after magpie became available); unreachable after 5 min of silence while hermit sees the sandbox alive | 8.8 |
| Availability gap that resets `available_since` | 20 s without a tick | 8.8 |
| Cancel confirmation | 30 s | `stop.forced` |
| Stuck single cc write (supervisor side) | 10 min while the heartbeat succeeds | `cc.write_stuck` |
| Driver claim | 30 s, renewed every 10 s | takeover |
| Limbo | 10 s of no operation on the file, or the turn's end | `retract` |
| Moved-limbo match window | 10 min | expires |
| Live frames kept | until 60 s after the turn ends | deleted |
| Recently deleted | 30 days | purge |
| Blob GC grace | 24 h | — |
| Unattached upload | 24 h | deleted |
| Change feed retention | 30 days | 410 `cursor_expired` |
| Idempotency (`cc_ops`, `report_ids`, `clientMessageId`, `idempotencyKey`) | 24 h / 24 h / for ever / for ever | — |
| Blob read cache | 256 MiB per replica | — |
| Concurrent image decodes | 4 per replica | queued |
| Postgres pool | 12 connections per replica + 1 LISTEN + 1 leader (R11) | — |

---

## 18. Deployment and operations

### 18.1 Build

Built and tested by aviary's `ci.yml` (platform 6, R17): `cargo test -p magpie -p magpie-api` against a Postgres
service container and a MinIO container; the image `ghcr.io/de0ch/magpie:<sha12>` (Rust build stage; runtime
`debian:bookworm-slim` with libheif and ca-certificates only, non-root uid 10001), public (R21). Never cancelled
mid-run.

### 18.2 YYC manifests (DE0CH/YYC `yyc/deploy/magpie.yaml` and `magpie-pg.yaml`, applied by Flux)

- Deployment `magpie`, namespace `yyc-ai` (R14), as platform 9.5: `args: [serve]`, 2 replicas, HPA 2-6 on CPU 70%;
  RollingUpdate `maxSurge 1`, `maxUnavailable 0`; PDB `minAvailable 1`; topology spread by hostname; readiness `GET
  :9090/readyz` every 3 s, liveness `:9090/healthz` (period 20 s, failureThreshold 6); `preStop: sleep 5`;
  `terminationGracePeriodSeconds 75`; resources per platform 16.1 (200m / 2 CPU, 1536Mi, `/tmp` emptyDir 1Gi);
  `readOnlyRootFilesystem`, drop ALL, seccomp RuntimeDefault, no ServiceAccount token, `enableServiceLinks: false`;
  priorityClass `serving`. No node affinity: magpie mounts nothing.
- Service `magpie`: port 80 → 8080 (9090 is not in the Service).
- NetworkPolicy `magpie-ingress`: 8080 from pods `pigeon`, `egress` (the gateway), `hermit`; 9090 from the kubelet
  and the metrics scraper. Egress from magpie: `magpie-pg-rw:5432` and `magpie-pg-r*` (namespace `db`), `hermit:8080`,
  the bucket endpoint (443), DNS.
- Secret `magpie` (Sealed Secret from store `yyc`, platform 12): `MAGPIE_DATABASE_URL`, `MAGPIE_S3_ENDPOINT`,
  `MAGPIE_S3_REGION`, `MAGPIE_S3_BUCKET`, `MAGPIE_S3_ACCESS_KEY_ID`, `MAGPIE_S3_SECRET_ACCESS_KEY`,
  `MAGPIE_ACCEPT_PIGEON`, `MAGPIE_ACCEPT_GATEWAY`, `MAGPIE_ACCEPT_HERMIT`, `MAGPIE_ACCEPT_ADMIN`, `MAGPIE_TOKEN_HERMIT`
  (store keys `YYC_TOKEN_PIGEON_MAGPIE`, `YYC_TOKEN_GATEWAY_MAGPIE`, `YYC_TOKEN_HERMIT_MAGPIE`,
  `YYC_TOKEN_ADMIN_MAGPIE`, `YYC_TOKEN_MAGPIE_HERMIT`, platform 3.7). These are bindings and credentials, not
  behaviour. `MAGPIE_POD` from the downward API names the replica in driver claims and logs.
- Database: its own CloudNativePG Cluster, because Claude Code's every transcript write now commits synchronously in
  this database, and the shared Cluster `pg` (namespace `db`) runs `instances: 1` (read live 2026-11-02: one instance
  `pg-1`, 2 CPU / 6Gi, 100Gi on `yyc-block`, shared with the news stream), so every restart, node drain or primary
  update of `pg` would freeze every running agent, and a later asynchronous replica could lose acknowledged writes on
  failover:
  ```yaml
  apiVersion: postgresql.cnpg.io/v1
  kind: Cluster
  metadata: {name: magpie-pg, namespace: db}
  spec:
    instances: 3
    primaryUpdateStrategy: unsupervised
    primaryUpdateMethod: switchover        # planned restarts promote a standby first, never stop the primary
    postgresql:
      synchronous: {method: any, number: 1, dataDurability: required}   # commit waits for one standby
      parameters: {max_connections: "150", shared_buffers: "256MB"}
    bootstrap: {initdb: {database: magpie, owner: magpie, secret: {name: pg-role-magpie}}}
    storage: {size: 10Gi, storageClass: yyc-block}
    resources: {requests: {cpu: 250m, memory: 1Gi}, limits: {memory: 1Gi}}
    affinity: {podAntiAffinityType: required, topologyKey: kubernetes.io/hostname}
    plugins:                               # the same barman-cloud ObjectStore as Cluster pg (live: pg-pitr)
      - {name: barman-cloud.cloudnative-pg.io, isWALArchiver: true,
         parameters: {barmanObjectName: pg-pitr, serverName: magpie-pg}}
  ---
  apiVersion: postgresql.cnpg.io/v1
  kind: ScheduledBackup
  metadata: {name: magpie-pg-nightly, namespace: db}
  spec: {cluster: {name: magpie-pg}, method: plugin, pluginConfiguration: {name: barman-cloud.cloudnative-pg.io},
         schedule: "<the same as pg-nightly>"}
  ```
  `dataDurability: required` means a commit waits for a standby; with both standbys down writes pause (503, retried)
  rather than risk loss. Three instances on three nodes fit the cluster's current free memory (2026-11-02 `kubectl top
  nodes`: large-worker1 50%, large-worker2 28%, small workers 50-63% used); no new server. The new paid resource is
  the three 10 GB Hetzner volumes: 30 GB × EUR 0.0572 net = EUR 1.72 per month net, EUR 2.06 per month with the
  account's 20% VAT (Hetzner Cloud API `GET /v1/pricing`, `volume.price_per_gb_month`, read 2026-11-02). Decided by
  Deyao (decision 2, 2026-11-02). `pg-ha.md` 4 adds to this manifest (DESIGN 12 D16): the same `imageName` as `pg`,
  `enableSuperuserAccess: false`, priority `serving`, `failoverQuorum: true`, `nodeSelector pool=apps`, a
  ScheduledBackup `magpie-pg-nightly` at 03:15 HKT from a standby, and the ingress policy by address.
  Backups: `pg-dump-daily` adds `magpie` against `magpie-pg-rw`; base backups and WAL archiving to the ObjectStore
  `pg-pitr` exactly as Cluster `pg` (PITR 3 days). `pg-ingress`-style policy `magpie-pg-ingress` admits only magpie pods and the dump job.
- Bucket: `yyc-ai-magpie` in YYC's Hetzner project (fsn1), private, no public access, no versioning, created with the
  project's existing S3 credential (inside the account's included storage; about 150 MB today).

### 18.3 Zero-downtime rollout

- New replicas become ready only after migrations are applied (by whichever replica wins the migrate lock) and LISTEN
  is up. Migrations are expand-only within a release, so the old replicas keep working against the new schema.
- SIGTERM (platform 3.3): `/readyz` answers 503 at once (the Service drops the pod during the 5 s preStop); stop
  accepting new requests; answer every held long-poll at once with what it has (pigeon and the supervisor re-poll on
  another replica, resuming by row seq, change seq and `cmdSeq`); finish in-flight requests (up to 30 s); release
  driver claims (`driver_until = now()`) and the leader lock (the hermit feed and the jobs move to another replica
  within 10 s, resuming from the stored cursor); close the pool; exit.
- No state lives in a replica beyond in-flight requests, so a rollout never cuts a turn: the turn runs in the
  supervisor, its writes retry onto another replica, and pigeon's polls resume by seq.
- The database: CNPG rolls `magpie-pg` instance by instance with a switchover before the primary (seconds of 503,
  retried); the operator's own upgrades the same way.

### 18.4 Observability

JSON logs with `conversation` (uuid), `turn`, `seq`, `rowSeq`, `caller`, `person_key`, `route`, `request`, `ms`;
never message contents or emails. Stable messages: `turn admitted`, `turn ended`, `sandbox acquisition failed`, `cc
write`, `cc rewrite`, `retract`, `epoch bumped`, `lease granted`, `lease refused`, `share created`, `share revoked`,
`conversation purged`, `outbox failing`, `blob upload failing`, `cc write stuck`, `listen reconnect`, `reconstruct
mismatch`, `watchdog`, `heartbeat reconciled`, `hermit feed lag`. The conversation uuid is the correlation id shared
with hermit's and egress's logs (platform 15.2).

### 18.5 Sizes today and headroom

Live data (2026-11-02): 26 transcripts, 9,648 lines, 232 MB, of which 202 MB are tool-result lines and about 195 MB
base64 that deduplicates to about 100 MB of blobs; 28 conversations; 9 shares. Expected magpie footprint: Postgres
under 100 MB (rows add a few MB), bucket about 150 MB including sent files and thumbnails. Load: about 10 concurrent
staff; 2 replicas are idle most of the time.

### 18.6 Backups and what deletion means

Postgres `magpie` is in the daily locked pg_dump (30 days, Object Lock) and the cluster's PITR (3 days). Bucket
objects are not versioned: after a purge and the 24 h GC grace, the bytes are gone; restoring an older database dump
gives rows whose blobs may be missing (reads then answer 404 for those blobs). This is the documented meaning of
"deleted for good". `/workspace` backups (the JuiceFS hourly metadata dump) are hermit's, not magpie's.

---

## 19. The one-off importer

`magpie import plan|run|verify|catch-up` (R15; the `magpie-import` library linked into the magpie image, deleted
after the switch). Its loop, its tables (`import_sources`, `import_shares`, `import_control`), the phases, the
release rule and the switch are the cutover spec's section 7 and 6; this section keeps only what magpie's own model
fixes. The importer writes through magpie's library, so events, rows, blobs and cc files are exactly what a live
write would make.

### 19.1 Mapping

| Source | magpie |
|---|---|
| `users.username` | `persons` (`person_key` recomputed and checked against the schema name) |
| `sessions` row | `conversations` with the same id, `migrating = true`, `origin {"kind": "import"}`; `custom_name → title`; `name_source` kept (null stays overwritable by a later title); `isArchived = 1` with `deleted_at` → `deleted_at`; `provider_session_id`; `model` |
| text timestamps | both formats (`YYYY-MM-DD HH:MM:SS` UTC and ISO) to timestamptz; the 1970 placeholder → the transcript's first record time |
| disk-discovered row (`session_id = provider_session_id`) | kept with that id |
| transcript `projects/-workspace/<psid>.jsonl` | a lines file in the conversation's scope at the same path (`main_transcript` set first), ingested with the live classifier and projection; `<psid>/tool-results`, `<psid>/subagents/**` as their kinds |
| `projects/-workspace/memory/**` | per conversation (decision 6): `plan` counts the files (zero today, inventory A8.F18); any that exist are copied into the cc scope of each of the person's imported conversations at import and again at that conversation's release (cutover 7.4.2) |
| `jarvis_failed_turns` | `user.sent` (if `user_text`) and `turn.ended` failed, placed after every transcript event not later than `user_at`/`error_at`; the error sentence mapped back to its code (unknown → `sandbox.failed` with the text as message) |
| `sent/<conv>/<toolUseId>/<rel>` | blobs + `sent_files` + a `files.sent` event placed after the SendUserFile tool_use; caption from the tool input |
| a completed, non-error SendUserFile call with no `sent/<conv>/<toolUseId>/` folder (sends before the old hook kept snapshots, up to 2026-10-22) | the same, from the files its result names (`toolUseResult.attachments[].path` under `/workspace`) as they are in `ws/<conv>/` at import, timed at the result line: the old stack shows those names from the result and downloads the live workspace file (`openSentFile`'s fallback); a file no longer there is left out, a call with none gets no event |
| `ws/<conv>/uploads/<name>` named by an imported prompt's `<files_input>`/`<image_uploads>` block | blobs, attached as `meta.attachments` of that prompt event (a missing file: `missing: true`) |
| `cc_shared.shares` live rows | `shares` with the same id; `upto_seq` = the seq of the last event whose line lies in the share's frozen `transcript.jsonl` when that file is a byte prefix of the live transcript; otherwise the frozen transcript is imported as a hidden scope and the share points at a deleted, hidden copy of the conversation; `share_rows` projected from those events; the snapshot scope from the frozen transcript and `session/`; `workspace_id = uuidv5(SHARE_NS, "share:" + id)`, the clone hermit's import pass makes from `users/<key>/shares/<id>/ws` (cutover 7.4.4, hermit 22.3) |
| revoked shares | rows kept as revoked, no data |
| `ops_events` view_as | `view_as_log` |
| `cloudcli-assets/*`, `superseded_provider_sessions` (0 rows), drafts (0), scheduled (0), preferences, notification and push tables, `scan_state`, `projects`, `api_keys`, `app_config` | not imported (section 22 says why) |

### 19.2 Byte-exact verification

After writing a conversation, the importer reads every file back through magpie's own reconstruct path and compares
SHA-256 with the source file (cutover 7.7). Any mismatch fails that conversation and blocks its release. The census
(453 images, lines up to 1.1 MB, torn tails if any) is the acceptance set. It also checks that the conversation's rows
equal a fresh projection of its events (the projection is deterministic).

### 19.3 Display fixes applied once, at import

These change only `events.data`/`meta` (and therefore rows), never the stored line bytes, so `--resume` still reads
exactly what Claude Code wrote:
- the legacy prefix `[System note: your sandbox was reset since the last message: …]` (6 occurrences in one
  transcript) is removed from `meta.displayText`;
- `<image_uploads>` and `<files_input>` blocks are stripped from the displayed prompt and become `meta.attachments`;
- pasted-content wrappers are unwrapped in `meta.displayText`.
The shims in today's code (`jarvis-history.ts` etc.) are not ported.

Intended differences from the old side's render (cutover 10.2 R6), besides the fixes above:
- a conversation whose transcript the old child will not read (`transcriptTooLongToOpen`: the file times the heap
  factor is more than the child's free heap, so the old side shows one `tooLong` error row instead of the history; in
  the 2026-11-05 parity run a103219b-d82c-45ea-b6c0-e477a17d746f, which magpie shows as its 1,135 rows) is shown in
  full: magpie reads a conversation's rows from the database page by page, with no limit on the transcript's size
  (baseline in 20: an 87 MB transcript read under 5 s), which is the fix `tooLong`'s own text promises staff;
- rows stand in transcript order (the order of the lines in the file, which is the order Claude Code itself reads on
  `--resume` and the order magpie's rows and their seqs take live), where the old side sorts the reloaded history by
  each line's `timestamp` (`claude-sessions.provider.ts` `fetchHistory`: `sort` by `timestamp`). The two differ where
  Claude Code wrote a line after a line with a later timestamp: every compaction (the summary line is stamped before
  the `compact_boundary` line that precedes it in the file, so the old side shows the summary first and magpie shows
  the "Compacted · …" row first) and a prompt queued while a reply was being written. In the 2026-11-05 parity run
  these are 7 rows of 5 conversations: two in a103219b (shown in full, above) and these four:
  cc2cef8f-43a8-4c06-88cd-aeec08c25343 (parity rows 63-64, from 0),
  4276202a-6d22-48e5-a45b-476b5e93441d (159-160), d6ccb4b7-f125-4aa1-978a-7050bdae51b0 (205-206), all three a
  compaction, and f3b2a699-8b9f-4ae9-a278-917bf466323d (29-32, the queued prompt "？"); sorting magpie's rows by their
  `at` reproduces the old side's order in each. Moving a row before rows already issued is not done: a row's place is
  its row seq, which pigeon pages by and Paseo clients apply in order (7.4);
- a `SendUserFile` card of a send made before the sent-file snapshots (up to 2026-10-22) whose file is no longer in
  the workspace has no attachment (19.1's table: "a file no longer there is left out"); the old side shows the name
  from the result and its download fails. In the 2026-11-05 parity run one row, d6ccb4b7-f125-4aa1-978a-7050bdae51b0
  row 56.

Not imported (decision 5, 2026-11-02): the old chat UI's 37 conversations from 2026-10-20/21 in database `jarvis`
and the bucket prefix `yyc-ai-files/jarvis/` are archived (cutover 15.4) and then deleted.

---

## 20. Tests and acceptance

- `scan.rs`/`extract.rs`: property tests, random JSON with large strings, escapes, surrogate pairs, invalid JSON;
  reconstruct(extract(x)) == x for every input.
- `lines.rs`: a reference model (a plain byte vector) run against random sequences of append, overwrite, truncate,
  truncate-then-rewrite-tail, slow whole-file rewrites (writes spread over more than 10 s), rename-over, rename away
  from and back onto the main path, set-aside then fresh create, and torn tails; the reconstructed file must equal
  the model after every step, the visible events (minus retracted) must equal the classification of the model's
  current main-transcript lines, and a slow rewrite must emit no `retract`.
- Projection (`rows/`): a reference projector run over random event sequences (tool calls with late results, task
  notifications, sent files before and after their tool row, failed and canceled turns, retracts) must equal the
  incremental projection; `row_seqs` must cover `1..row_head` exactly once; `after` paging from every `S` must return
  every seq once.
- Claude Code fixtures: the real Claude Code binary in CI against a mock Messages API, writing through a test FUSE
  supervisor into magpie: a turn, a resume in a fresh sandbox (byte-exact read), a tombstone, a compaction, a
  subagent transcript (link fallback), a relocation with set-aside, cleanup refused on the transcript.
- **Claude Code release gate** (new, nightly and on every pin change): the FUSE + magpie harness above runs against
  every new Claude Code release before the sandbox image's pin moves (supervisor 22). It fails on any `unclassified
  path` log from the supervisor, on any change in the event and row counts of the fixture conversations, or on a
  reconstruct mismatch. The pin moves only on green.
- Turns: admission races (two concurrent sends → one turn, one 409), idempotent replays, cancel at every state
  (including `held` → `stop_tasks`), hermit answers mapped to codes (403, 409 `person_mismatch`, 503 `sandbox_busy`,
  `gone` with each release code), driver takeover after a killed replica, a `gone` feed event → `sandbox.lost` within
  seconds, a lost end report reconciled by the next heartbeat, `transcript_pending` ordering.
- Watchdog: magpie (all replicas) stopped for 6 min during a running turn, then started: the turn completes, no
  `sandbox.unreachable`, and the late transcript lines land before `turn.ended`. Database switchover mid-turn: no
  write lost, the turn completes.
- Lease: two live sandboxes for one conversation: only hermit's current one ever holds the lease; no restart loop.
- Writes: p99 <= 150 ms for a cc write with a 1 MiB image line and p50 <= 15 ms for a 10 KiB append, in-cluster
  with synchronous replication; the bucket made unreachable for 5 min: cc writes keep succeeding, staged blobs drain
  afterwards.
- Fan-out: two magpie processes on one Postgres; a long-poll on A sees writes made through B; the LISTEN connection
  killed mid-poll loses nothing.
- Contract (the pigeon contract suite, platform 8, runs against magpie): every M1-M17 route and error code of 16.1;
  a conversation that interleaves `title.changed`, `task.status`, `sandbox.*`, `ask.*` and `retract` events streams
  through M9 with zero refetches (no false gap) and exactly one `timeline_replaced` per epoch change; `turn` changes
  arrive with `timelineSeq` >= the turn's last row.
- Harness scenarios (platform 7.8): E2 (live frames precede the durable row, superseded without a duplicate); a new
  scenario "reconnect mid-turn across a compaction": close the socket during a turn whose transcript Claude Code
  compacts, reopen, and assert no gap loop and no duplicate row; E7, E9 (rollouts of magpie during a turn).
- Shares: freeze excludes a running turn; view equals the conversation's rows at the freeze; revoke and purge give
  404; soft delete gives 404 and restore brings it back; continue gives an independent conversation whose
  `--resume` works and whose workspace is the frozen clone.
- Ported acceptance from today's suites: Jarvis-mode server tests (files panel, sent snapshots from the send not the
  changed file, delete releases the sandbox and removes files, names, failed turn keeps the message then the error,
  stop while preparing), shares tests, title tests (cleanTitle cases, rename wins, failure keeps provisional), image
  attachment tests, the Paseo-bridge paging tests (run against pigeon over magpie), view-as checks (record with the
  send and the cancel, refusals), the share viewer check, and the local end-to-end harness (fake hermit, mock
  Anthropic, real supervisor).
- Baselines to meet: title visible 3-4 s after send on a warm sandbox (today); share link under 1 s plus hermit's
  clone (today 6.4 s); history page p95 under 150 ms; an 87 MB transcript read under 5 s.

---

## 21. Decisions taken in this spec, and what the other specs must provide

### 21.1 Decisions (best reading of the brief; easy to change before implementation)

1. Sent-file snapshot bytes live in magpie's bucket, uploaded by the supervisor from `/workspace`, rather than as
   JuiceFS clones made by magpie: JuiceFS is YYC's hermit implementation, and the brief forbids YYC logic outside
   hermit/egress/tools. Workspace-level copies (share freeze, continue) are hermit workspace clones
   (`POST /v1/workspaces/{wid}/clone`), metadata-only in YYC.
2. Only the main transcript's user/assistant lines become content events; everything else Claude Code writes is
   stored for byte-exactness without events.
3. magpie owns the timeline projection (7.3): rows with a dense row seq and a stable epoch, written in the event's
   transaction; pigeon maps one row to one Paseo row. (Review round 1; section 24 says why the alternative was not
   taken.)
4. Live frames exist (7.5): ephemeral, never events, superseded by the durable row.
5. Claude Code session ids and uuids are not rewritten on continue (per-conversation namespaces make it unnecessary).
6. A share of a conversation in Recently deleted answers 404 until restore.
7. Titles: the model call is executed by the supervisor through `model`; the title instructions are hermit's instance
   content; up to 3 attempts instead of 1.
8. Error texts, the untitled placeholder and the locale are pigeon's; magpie returns codes only.
9. Dropped for lack of any client: fork, edit/rewind, scheduled messages, drafts and queued messages, server-side
   preferences, web push and notification preferences, cross-conversation full-text search.
10. AskUserQuestion/ExitPlanMode: magpie stores asks and answers generically; the supervisor disallows those tools in
    YYC (supervisor 24 item 6), so the relay is expected to stay unused.
11. Soft delete releases the sandbox (hermit 3.6.4); a running turn ends with it.
12. Offboarding never deletes data automatically; an operator purges a leaver through hermit's admin route.
13. The main transcript is a path, not a file identity (6.2).
14. One retry rule for cc writes in both specs (6.12).
15. magpie's database is its own three-instance synchronous CNPG cluster (18.2), decided (decision 2).

### 21.2 What magpie needs from the other specs

- **hermit**: `PUT /v1/conversations/{id}/sandbox`, `GET …/sandbox?waitMs&afterVersion`, `POST …/sandbox/release
  {reason: deleted|offboarded|requested}`, the feed `GET /v1/sandbox-events?after&limit&waitMs` (3.6), with
  `lastActiveAt` on the Sandbox object reflecting the supervisor's activity reports; `GET /v1/people/{email}`;
  `GET /v1/workspaces/{wid}/files?path=` (Range), `POST /v1/workspaces/{wid}/clone {from}`, `DELETE
  /v1/workspaces/{wid}` (3.8). Changes in hermit's spec: drop the sentence in 3.8 that magpie stages uploads with `PUT
  …/files` (the supervisor writes uploads; keep that route only if another caller needs it); drop `supervisor.token`
  from the Sandbox object or mark it unused by magpie (magpie never calls :7070); replace the consumed
  `GET /conversations/exists` with magpie's `POST /v1/hermit/workspaces/exists` (9.7); add the operator route
  `POST /v1/admin/people/{email}/purge` that forwards to magpie (9.6); `GET /ops/people`, `GET /ops/view-as` become
  `/v1/ops/people`, `/v1/ops/view-as` (R19). hermit owns every JuiceFS mechanic magpie's coverage row 270 points to:
  the volume format (TrashDays 0, no encryption, bucket `yyc-ai-files/yyc-ai`), the hourly metadata dump (taken by
  hermit's mount client, with no mount using `--no-bgjob`), the client background jobs, the CSI HelmRelease, static
  PV and mount pods, readiness under a mount-pod restart, and the ownership of `ws/<id>` and the uid of
  `DELETE /v1/workspaces` (review point 1; hermit 15 and its coverage rows A18.F0, F5, F11, F13, F16-F19).
- **The sandbox gateway (YYC egress)**: a route for the fixed name `magpie` to `http://magpie`, `/v1/sandbox/**`
  only, with its pair token and verified `Aviary-Sandbox-Id`/`-Conversation`/`-Person`; long-lived responses (25 s
  polls); bodies up to 64 MiB; the old `sent-files` route deleted.
- **supervisor**: the FUSE mapping of 6.1 and the errno mapping and retry rule of 6.12 (unbounded under general
  unavailability, `cc.write_stuck` after 10 min for one op while the heartbeat succeeds, the resync and offset repair
  from its write-through cache); `turnId` on every cc write; reports never dropped (retry while the lease is valid);
  the end report's `lastUuid` and waiting for its writes with no 5 s cap (supervisor 8.2); the heartbeat's `lastTurn`;
  `origin: "agent"` (was `cli`); `askId`/`toolUseId` on asks; camelCase bodies and `waitMs` on the command poll; on
  `not_current_sandbox` stay unready and re-try the lease every 30 s without exiting; on 409 `turn_not_active` (or a
  404 from an older magpie) for `/live`, drop frames and never retry; the `stop_tasks` and `kill {reason}` commands;
  `run.oom` and `cc.write_stuck` codes; upload materialisation into `/workspace/uploads`; executing `title` commands.
- **pigeon**: the `Aviary-*` headers; M1-M17 exactly as 16.1 (paths under `/v1/`); the pure Row → PaseoRow mapping
  only (7.8), never a projection of events; `migrating` on its Conversation type and `conversation_migrating` in the
  error lists of M5-M8, M11 and M16 (retry per cutover 6.5); `liveAfter` on its timeline poller and a mapping of
  `LiveState` to an in-progress assistant/reasoning row that the durable row with the same `(messageId, blockIndex)`
  replaces (never a second `assistant_message`), dropped at the turn's end, with `reset` handled as a full repaint of
  that row; a contract test that runs the app's real timeline reducer (`paseo/packages/client`) over a live turn and
  asserts the rendered text equals the durable text with no duplicate; sentences for `run.oom`, `cc.write_stuck`,
  `sandbox.timeout`, `legacy.cut`; string-table keys are the dotted codes of 8.9.
- **cutover**: 18.2 points to decision 3 (magpie projects; M9 is magpie's); 17.4's "supervisor: no change" becomes the
  supervisor list above; its importer writes through magpie's projection (19).
- **platform**: the R26-R31 rows of 21.3; 7.8 E2 as worded holds with 7.5; the new harness scenario of section 20;
  11.1/11.4 move `magpie` to `magpie-pg` (decided).

### 21.3 Rows the platform reconciliation table must add

| # | Topic | Decision |
|---|---|---|
| R26 | cc write retry | unbounded under general unavailability; `cc.write_stuck` after 10 min for one op while the heartbeat succeeds (magpie 6.12, supervisor 9.5 both say this) |
| R27 | Timeline projection owner | magpie: `rows` with dense `rowSeq` and stable `epoch`, written with the event; M9/M10/M16 serve rows; pigeon maps Row → PaseoRow only (magpie 7.3) |
| R28 | Workspace snapshots in YYC | hermit `snapshot` stays 501; share freeze and continue use only `POST /v1/workspaces/{wid}/clone` (magpie 12, supervisor 16.3 drop "behind the snapshot calls") |
| R29 | `link(2)` in magpie scope | EPERM in both specs |
| R30 | Turn origin enum | `person` \| `agent` \| `import` everywhere (`cli` and `user` gone) |
| R31 | magpie's database | own Cluster `magpie-pg`, 3 instances, synchronous (`any 1`, `required`), switchover updates (magpie 18.2), decided (decision 2) |

### 21.4 Decided by Deyao (2026-11-02)

1. `magpie-pg`'s three 10 GB volumes (EUR 2.0592 a month with VAT): yes (decision 2).
2. Claude Code's auto-memory: per conversation (decision 6).
3. A leaver's conversations: deleted only by an operator's person purge (9.6), never automatically (decision 7).

Nothing in this spec is open for Deyao any more.

---

## 22. Coverage table

Every inventory feature whose `target_home` names magpie (296). "Covered" names where this spec handles it; "not
magpie" means the feature names magpie only as a data source for another component's part, and says what magpie
provides; "dropped" says why.

| # | Feature (area) | Status | Where / why |
|---|---|---|---|
| 0 | userKey derivation (0) | covered | 3.2 person_key kept bit-identical on persons |
| 1 | View-as start: POST /api/jarvis/view-as (0) | covered | 13 POST /v1/view-as (M17; pigeon owns the token) |
| 2 | View-as read-only enforcement (0) | covered | 13 refusals and record-with-send; pigeon gates methods |
| 3 | Per-person CloudCLI process spawn (0) | dropped | no per-person processes; magpie is a shared service (1, 18.2) |
| 4 | Child output logging (0) | covered | 18.4 structured logs |
| 5 | Running-chat probe (0) | covered | 9.1 run.state per conversation, 8.5 supervisor reports |
| 6 | Child heap limit (0) | dropped | no full-history load; pages by seq (7.3) |
| 7 | purgeWake: daily start to empty Recently deleted (0) | covered | 15 purge job (leader), no process wake |
| 8 | Rendezvous hashing across replicas (0) | dropped | no affinity; single writer per conversation via row lock and lease (4, 6.9) |
| 9 | Peer holder discovery (rollout-safe owner) (0) | dropped | stateless replicas; supervisor owns the live run, streams resume by seq (7.4) |
| 10 | HTTP proxy to the local process (0) | not magpie | pigeon routes REST; magpie's routes in 16.1 |
| 11 | Graceful drain on SIGTERM / SIGINT (0) | covered | 18.3 drain (readiness off, reconnect frames, claims released) |
| 12 | Rollout shape (zero downtime) (0) | covered | 18.2 rollout shape for magpie |
| 13 | Sent-files snapshot endpoint (0) | covered | 11.2 sent-file snapshots via the gateway into magpie blobs |
| 14 | POST /api/assets/files uploads (passed through) (0) | covered | 10.1 POST /v1/uploads |
| 15 | NetworkPolicies around ui (0) | covered | 18.2 magpie-ingress / egress policies |
| 16 | Jarvis mode switch and the two modes (1) | dropped | no modes in magpie (1: no switches) |
| 17 | Instance config read (product name, locale, model, label, title model) (1) | covered | locale/product moved out (21.6); title prompt from hermit (9.4); model from transcript (9.1) |
| 18 | Per-user process bootstrap: single user, single project (1) | dropped | no per-person schema; ownership check replaces the bootstrap guard (3.2) |
| 19 | Per-user Postgres schema cc_<userKey> and the SQLite-compat layer (1) | covered | 4 one database, rows keyed by person |
| 20 | Request gate: X-Jarvis-User must equal the owner (1) | covered | 3.1-3.3 caller tokens + person header |
| 21 | View as (admin acting as staff) in this process (1) | covered | 13 view-as enforcement and audit |
| 22 | 'Preparing the workspace' status frames and Stop while preparing (1) | covered | 8.3 sandbox.preparing/ready events and `sandbox` changes; 8.6 cancel while preparing |
| 23 | Running Claude Code inside the sandbox (/proc protocol) (1) | not magpie | supervisor runs Claude Code; magpie's command queue 8.4 |
| 24 | Fixed model: one instance model, no picker, no effort (1) | covered | 9.1 model recorded from the transcript; label is pigeon/hermit |
| 25 | Permission mode and settings in the sandbox (1) | not magpie | supervisor launch flags; turn payload carries none (8.4) |
| 26 | Conversation titles from the title model (inside the sandbox) (1) | covered | 9.4 titles, executed by the supervisor through model |
| 27 | Provisional names and the name precedence (1) | covered | 9.2 provisional title, 9.4 precedence, 6.5 Claude Code titles |
| 28 | Conversation list, details, running list (1) | covered | 9.3 list/details, 9.1 run state |
| 29 | Recently deleted (soft delete, restore, list) (1) | covered | 9.6 soft delete/restore, 9.3 state=deleted |
| 30 | Permanent delete (force) and its side effects (1) | covered | 9.6 purge incl. shares, sandbox, workspace via outbox |
| 31 | Recently-deleted purge job (30 days) (1) | covered | 15 purge job |
| 32 | Fork a conversation (copy of /workspace and sent snapshots) (1) | dropped | fork has no UI (docs/paseo-ui.md:81); continue (12.4) is the same mechanism |
| 33 | Failed-turn records merged into history (1) | covered | 8.2 user.sent stored first + turn.ended failed events |
| 34 | Readable errors shown to staff (zh-HK / en) (1) | covered | 8.9 error codes; pigeon localises |
| 35 | Budget / 402 handling (1) | covered | 8.9 meta.error model.budget carries egress's message |
| 36 | Uploads/attachments staging into /workspace/uploads (1) | covered | 10.3 uploads as blobs, supervisor materialises /workspace/uploads |
| 37 | Images: model-ready copy + original saved (1) | covered | 10.2 model copy at upload |
| 38 | Sent files (SendUserFile) list = files panel (1) | covered | 11.1 files panel from sent_files |
| 39 | File download / inline preview (/files/raw) with symlink-safe open (1) | covered | 11.3 files/raw (sent from blobs, live via hermit) |
| 40 | Sent-file snapshot (copy-on-write at send time) (1) | covered | 11.2 snapshot acked once stored |
| 41 | Shares: create (freeze), list, revoke (1) | covered | 12.1-12.2 create/list/revoke |
| 42 | Shared page view /s/<id> and its file downloads (1) | covered | 12.3 share view and files; pigeon routes /s/<id> |
| 43 | Continue a shared conversation (1) | covered | 12.4 continue |
| 44 | Share revocation on conversation delete (1) | covered | 9.6 purge revokes shares |
| 45 | Shares table auto-creation (1) | covered | 4 proper migrations |
| 46 | Run registry, seq numbering, replay, single-run lock (1) | covered | 7 events with seq per conversation; 8.2 single admission |
| 47 | Provider-id mapping and transcript path at first sight (1) | covered | 8.5 providerSessionId/transcriptPath report, 6.2 promotion |
| 48 | Activity ordering: touch on run start/complete (1) | covered | 8.2/8.5 activity_at touches |
| 49 | session_upserted / session_removed broadcasts and cross-replica NOTIFY relay (1) | covered | 7.6 change feed (conversation_upserted/removed) + 14 fan-out |
| 50 | Transcript sync in Jarvis mode (startup only, plus orphan prune) (1) | dropped | rows are the truth; no scan (6, 9) |
| 51 | History loading for Jarvis transcripts (1) | covered | 7.4 row paging; 19.3 legacy display fixes applied at import |
| 52 | Edit an already-sent message (chat.edit-send) (1) | dropped | edit-send only from CloudCLI /ws (dropped); retract/meta.replaces cover rewrites (6.7) |
| 53 | Abort / stop-task / permission response (1) | covered | 8.6 cancel (M8), 8.4 answer_ask; stop-task = `stop_tasks` when a cancel arrives during a hold |
| 54 | Detached turns: scheduled messages and local deliver (1) | dropped | no scheduled turns/local deliver client (Jarvis 4 open) |
| 55 | Health endpoint and router-driven process lifecycle (1) | covered | 16.5 health; 18.3 rolling deploys |
| 56 | Web push / notifications per user (1) | dropped | no push in production (gap-3 #16) |
| 57 | User preferences and drafts (1) | dropped | Paseo keeps settings and drafts on the device; no client reads them |
| 58 | get_providers_snapshot_request (providers/models) (2) | not magpie | pigeon/hermit: model label; magpie gives per-conversation model (9.1) |
| 59 | fetch_workspaces_request (conversation list) (2) | covered | 9.3 list M1 (pigeon shapes workspaces) |
| 60 | fetch_agents_request (agent list) (2) | covered | 9.3 list + 9.1 run (pigeon shapes agents) |
| 61 | fetch_agent_request (2) | covered | 9.3 GET /v1/conversations/{id} incl. deleted |
| 62 | fetch_agent_history_request (Recently deleted / History page) (2) | covered | 9.3 state=deleted&q= (paged, no N+1) |
| 63 | fetch_agent_timeline_request (timeline paging, epochs, seq) (2) | covered | 7.4 M9 tail/before/after over rows, stable epoch, dense row seq |
| 64 | Timeline row shape and projection (2) | covered | 7.3 magpie projects protocol-neutral rows in the event transaction; pigeon maps Row → PaseoRow only |
| 65 | Sent-file metadata on SendUserFile calls (2) | covered | 7.3 rule 6: the SendUserFile tool row carries `sentFiles` |
| 66 | agent.timeline.list_prompts.request (prompt index) (2) | covered | 7.5 prompts |
| 67 | send_agent_message_request (send) (2) | covered | 8.2 turn admission, 10 uploads |
| 68 | workspace.create.request (new conversation + first message) (2) | covered | 9.2 create with first turn, idempotent across sockets |
| 69 | Rename: update_agent_request and workspace.title.set.request (2) | covered | 9.5 rename |
| 70 | Delete / archive: archive_workspace_request, archive_agent_request, delete_agent_request (2) | covered | 9.6 soft/permanent delete |
| 71 | Restore: refresh_agent_request (2) | covered | 9.6 restore |
| 72 | Live list updates: agent_update / workspace_update (2) | covered | 7.6 change feed M12 |
| 73 | Live timeline stream: agent_stream timeline events (2) | covered | 7.4 M9 long-poll `after=<rowSeq>&waitMs`; 7.5 live frames |
| 74 | Turn lifecycle: turn_started / turn_completed / turn_failed / turn_canceled (2) | covered | 8.1 turn.started/turn.ended events |
| 75 | Following runs started elsewhere (2) | covered | 7.4 any replica answers polls by row seq; agent-started turns 8.5 |
| 76 | Hub as in-process CloudCLI chat client (2) | dropped | replaced by REST + events (7, 8) |
| 77 | Generation/epoch reset on process restart (2) | covered | 7.3 epoch is a column: stable across restarts and rollouts, changes only when visible history is rewritten |
| 78 | Inline image and uploaded-file handling (2) | covered | 10 uploads + images as blobs |
| 79 | Share snapshot reuse of SessionTimeline (2) | covered | 12.1/12.3 share_rows frozen at the share, served by the same row shape |
| 80 | REST surface the app uses beside the socket (2) | covered | 16.1 magpie routes behind pigeon's paths |
| 81 | POST /internal/cc/release: conversation deleted (3) | covered | 9.6 outbox release_sandbox |
| 82 | POST /internal/cc/view-as: audit record of an admin viewing or sending as a staff member (3) | covered | 13 view_as_log |
| 83 | user_key derivation (~/.claude folder key) (3) | covered | 3.2 person_key on persons |
| 84 | Sent-file snapshots hook (PreToolUse on SendUserFile) (3) | covered | 11.2 (hook via supervisor) |
| 85 | JuiceFS mounts for /workspace and ~/.claude (3) | covered | 6 ~/.claude backed by magpie; /workspace is hermit's |
| 86 | GET /internal/ops/ — operator page HTML (3) | not magpie | ops page is hermit's (pigeon forwards); magpie feeds people and view-as (16.4) |
| 87 | GET /internal/ops/snapshot?tz=<zone> — operator data (3) | covered | 16.4 /v1/ops/people and /v1/ops/view-as for hermit's snapshot |
| 88 | Ops section: people (conversations per staff member) (3) | covered | 16.4 /v1/ops/people |
| 89 | Ops section: view-as log (3) | covered | 13 /v1/admin/view-as-log |
| 90 | ops_events recording (3) | covered | 13 view_as_log (sandbox_start stays hermit's) |
| 91 | Postgres schema migrations (shared by all roles) (3) | covered | 4 magpie's own migrations; old UI tables not imported (19.3, cutover 20) |
| 92 | Conversation list (workspaces + agents directory) (4) | covered | 9.3 list + 7.6 change feed |
| 93 | Directory sync cursors (4) | covered | 7.6 durable change-feed cursors (generation + seq) |
| 94 | Agent timeline fetch (paging) (4) | covered | 7.4 row paging |
| 95 | Live timeline subscription and seq gating (4) | covered | 7.1 contiguous seq; pigeon gates |
| 96 | Timeline resume / epoch change handling (4) | covered | 7.4 resume by row seq; the epoch survives deploys |
| 97 | Load older history (4) | covered | 7.4 before=<seq>&limit=40 over rows |
| 98 | Chat outline (prompt index) (4) | covered | 7.5 prompts |
| 99 | Find in chat (web) (4) | covered | 7.5 search in conversation |
| 100 | New conversation (create workspace with first message) (4) | covered | 9.2 create |
| 101 | Rename conversation (4) | covered | 9.5 |
| 102 | Delete to Recently deleted (archive) (4) | covered | 9.6 soft delete |
| 103 | Recently deleted page (restore / delete for good) (4) | covered | 9.3 deleted list, 9.6 restore/permanent |
| 104 | Fetch one conversation (4) | covered | 9.3 GET one, deleted included |
| 105 | Agent snapshot fields the app reads (4) | covered | 9.1 Conversation object |
| 106 | File uploads for a message (4) | covered | 10.1 uploads, 50 MiB = app cap |
| 107 | User message attachments in history (4) | covered | 7.2 user.sent files/images with blob refs; 7.3 user rows |
| 108 | Tool call rendering (4) | covered | 6.5 event data; 5.5 text previews for large outputs |
| 109 | Sent-file cards (SendUserFile) (4) | covered | 7.2 files.sent, 7.3 tool row `sentFiles`, 11.3 downloads |
| 110 | Paths named in replies download (4) | covered | 11.3 live workspace read via hermit |
| 111 | Markdown images in replies (4) | covered | 11.3 workspace images now readable |
| 112 | Share a conversation (4) | covered | 12.1-12.2 |
| 113 | Shared conversation page /s/:id (4) | covered | 12.3 |
| 114 | sbx-init: managed Claude Code files from env (5) | not magpie | supervisor/hermit managed config; magpie not involved |
| 115 | /proc exec protocol (process start, stream, signal, kill) (5) | not magpie | supervisor; magpie's command/report API replaces /proc (8.4-8.5) |
| 116 | Claude Code launch spec from the UI (5) | covered | 8.4 start_turn payload (resume id, uploads) |
| 117 | SendUserFile snapshot hook (5) | covered | 11.2 |
| 118 | JuiceFS persistent folders + quota (5) | covered | 6 ~/.claude in magpie; /workspace quota stays hermit's |
| 119 | Route `sent-files` (identity-verified snapshot before SendUserFile) (6) | covered | 11.2 replaces the egress sent-files route (21: route deleted) |
| 120 | config.json: product, publicUrl, access (7) | not magpie | pigeon sign-in; magpie needs no locale (21.6) |
| 121 | config.json: models, defaultModel, titleModel, titleReasoning, systemPromptFile (7) | covered | 9.4 title model is the supervisor's small/fast model; no config in magpie |
| 122 | config.json: egress (7) | covered | 21 gateway route `magpie`; sent-files route removed |
| 123 | ui per-user process placement (rendezvous hashing + peer holds) (7) | dropped | no per-person placement |
| 124 | ui per-user process limits and idle stop (7) | dropped | no per-person processes |
| 125 | Recently deleted purge wake (background job) (7) | covered | 15 purge job |
| 126 | Sent-file snapshots route (egress → ui) (7) | covered | 11.2 |
| 127 | View as (admin impersonation, read-mostly) (7) | covered | 13 |
| 128 | JuiceFS volume yyc-ai via the CSI driver (7) | covered | 6 + 11: transcripts and sent files leave JuiceFS for rows + bucket |
| 129 | Operator page /ops (data sources) (7) | covered | 16.4 admin people/view-as for the aggregator |
| 130 | News-stream live dashboard reads the ops snapshot (7) | covered | 16.4 /v1/ops/* data for hermit's snapshot, which keeps the monitor contract |
| 131 | Postgres databases and roles for YYC AI (7) | covered | 18.2 database magpie, role, CR |
| 132 | Postgres backups (daily locked dump + PITR) (7) | covered | 18.2/18.6 magpie added to pg-dump-daily |
| 133 | claudecodeui jarvis-image workflow (UI image) (7) | covered | 18.1 magpie image workflow |
| 134 | Offline / old-UI rollback runbook (7) | covered | 19.3 legacy old-UI data left to Deyao (cutover 20) |
| 135 | Per-user ~/.claude folder on JuiceFS (shared by all of a user's conversations and sandboxes) (8) | covered | 6.1 per-conversation scope replaces the per-person folder |
| 136 | Host JuiceFS mount (sbx-jfs.service) (8) | not magpie | hermit host setup; ~/.claude no longer on JuiceFS |
| 137 | UI pod JuiceFS mount (/jfs, CSI) (8) | dropped | magpie mounts nothing (18.2) |
| 138 | HOME/.claude symlink per user process (8) | dropped | magpie serves data by REST |
| 139 | Transcript file projects/-workspace/<sessionId>.jsonl (8) | covered | 6.2 lines file + events |
| 140 | JSONL record types present (live census) (8) | covered | 6.3-6.5 every record stored opaquely, byte-exact |
| 141 | Image and toolUseResult duplication in transcripts (8) | covered | 5.1/6.4 dedupe by sha |
| 142 | Claude Code transcript append path (8) | covered | 6.6/6.10 append path, torn tail, ack after store |
| 143 | Resume reads the whole transcript (8) | covered | 6.11 fast full reads + supervisor cache |
| 144 | Saved tool outputs projects/-workspace/<sessionId>/tool-results/ (8) | covered | 6.2 byte files (pages) |
| 145 | Subagent and workflow transcripts (supported by the UI, absent today) (8) | covered | 6.2 lines files without events; link -> EPERM (6.8) |
| 146 | Live-session registry sessions/<pid>.json and <pid>.<sha256>.key (8) | not magpie | supervisor-local (6.1) |
| 147 | Shell snapshots shell-snapshots/snapshot-bash-<ms>-<rand>.sh (8) | not magpie | supervisor-local (6.1) |
| 148 | Retention sweep (.last-cleanup, cleanupPeriodDays) (8) | covered | 6.8 transcript unlink refused (EPERM) |
| 149 | Auto-memory dir projects/-workspace/memory/ (8) | covered | 6.1 per conversation (decision 6); a memory file is copied into each of the person's conversations at import (19.1) |
| 150 | Uploaded images cloudcli-assets/ (written by the UI, not Claude Code) (8) | covered | 10 uploads in blobs, not in ~/.claude |
| 151 | Fork of a conversation (SDK forkSession run in the UI pod) (8) | dropped | fork dropped (no UI); continue 12.4 |
| 152 | Share snapshot and 繼續對話 (continue from share) (8) | covered | 12.1/12.4 |
| 153 | History reading for the UI and the Paseo apps (8) | covered | 7.3-7.4 rows |
| 154 | Files panel and sent-file cards read the transcript (8) | covered | 11.1 from sent_files rows |
| 155 | Start-up session synchronisation and orphan pruning (8) | dropped | no scan; rows are the truth |
| 156 | Conversation search (8) | covered | 7.5 in-conversation search; cross-conversation full text dropped (no client) |
| 157 | Token usage and active model read from the transcript (8) | covered | 9.1 model and context_tokens from ingest |
| 158 | Deletion and Recently deleted purge remove only the .jsonl (8) | covered | 9.6 purge removes everything of the conversation |
| 159 | CloudCLI readers of user-level Claude config (not used by YYC today) (8) | dropped | no client; FUSE shows no user settings |
| 160 | Migration and checkpoint durability of ~/.claude writes (8) | covered | 6.10 ack after store; mtime kept (6.11) |
| 161 | Concurrent writers to one ~/.claude (8) | covered | 6.1 per-conversation namespace; shared files stay local |
| 162 | Background task tracker (task events) (9) | covered | 8.5 tasks report -> task.status + background |
| 163 | CLI-initiated follow-up turns (task completion, Monitor events, wake-ups) (9) | covered | 8.5 POST /v1/sandbox/turns origin agent, plus heartbeat reconciliation |
| 164 | Running list during a hold: GET /api/providers/sessions/running (9) | covered | 9.1 run.state held + background |
| 165 | Run-registry retention and subscribe while held (9) | covered | 7.4 rows by seq replace replay buffers |
| 166 | Scheduled (queued) messages wait for background work (9) | dropped | scheduled messages dropped (no client) |
| 167 | Paseo hub/timeline during a hold (9) | covered | 8.1 held state, task.status events, CLI turn boundaries |
| 168 | History reading of background launches and follow-up turns (fetchHistory) (9) | covered | 7.3 rule 3: task notifications folded into the launch tool row at write time |
| 169 | Live task_status frames (9) | covered | 8.5 tasks -> task.status |
| 170 | Failures during a hold (9) | covered | 8.7 hermit feed `gone` → sandbox.lost; 8.8; run errors as turn.ended |
| 171 | Platform-mode auth (first DB user) (10) | dropped | identity from pigeon headers |
| 172 | Bootstrap: one user + one /workspace project (10) | covered | 3.2 ownership check is the invariant; projects dropped |
| 173 | POST /api/assets/files (chat upload staging) (10) | covered | 10.1; 24 h GC of unattached uploads |
| 174 | /api/settings notification-preferences + push (vapid-public-key, subscribe, unsubscribe) (10) | dropped | no web push |
| 175 | /api/scheduled-messages (list/create/delete) + scheduled dispatcher (10) | dropped | no client |
| 176 | GET /api/providers/claude/models (10) | not magpie | pigeon/hermit instance info |
| 177 | POST /api/providers/sessions, GET sessions/:id/messages, DELETE sessions/:id(?force=true) (10) | covered | 16.1 REST conversation API; scripts ported (20) |
| 178 | Other provider routes still reachable (10) | dropped | no YYC client; deleted list, restore, rename kept in 9 |
| 179 | Legacy /ws chat socket (10) | dropped | /ws dropped; run semantics in 7-8 |
| 180 | Session synchronizer at start (all 4 providers) (10) | dropped | no scan; only Claude |
| 181 | Full argv the SDK builds for a YYC (Paseo) turn (11) | not magpie | supervisor builds the argv; magpie supplies resume id (8.4) |
| 182 | stdin user message (11) | covered | 8.4 start_turn payload with uploads/model copies |
| 183 | stdin control_request interrupt (chat.abort, Paseo cancel) (11) | covered | 8.6 cancel (records at once, interrupt command) |
| 184 | stdout control_request can_use_tool → canUseTool → permission_request (11) | covered | 8.5 asks report + 8.4 answer_ask |
| 185 | stdout control_request hook_callback (Notification hook) (11) | dropped | no notifications; ask events cover attention |
| 186 | AskUserQuestion / ExitPlanMode in YYC (bypassPermissions) (11) | covered | 7.2 ask.pending/ask.resolved; tool policy 21.1 decision 10 |
| 187 | Title call through /proc (runInSandbox) (11) | covered | 9.4 title command |
| 188 | Claude Code transcript lines the supervisor/FUSE must carry (2.1.304) (11) | covered | 6.3-6.5 every line kind stored |
| 189 | Per-user schema selection (cc_<userKey>) (12) | covered | 4 one database |
| 190 | Schema init + versioned migrations under advisory lock (12) | covered | 4 migrations under advisory lock |
| 191 | SQLite compat SQL functions sqlite_ts/julianday/datetime (12) | dropped | timestamptz; normalised at import (19.1) |
| 192 | SQL dialect translation layer (12) | dropped | native Postgres |
| 193 | users table (the one owner row) (12) | covered | 4 persons |
| 194 | sessions table (the conversation index) (12) | covered | 4 conversations |
| 195 | superseded_provider_sessions (12) | dropped | 0 rows, no rewind |
| 196 | jarvis_failed_turns (12) | covered | 19.1 imported as events |
| 197 | session_drafts (composer drafts + queued message) (12) | dropped | 0 rows; drafts on device |
| 198 | scheduled_messages (12) | dropped | 0 rows; no client |
| 199 | user_preferences (12) | dropped | no client reads it |
| 200 | user_notification_preferences (12) | dropped | all defaults; no notifications |
| 201 | vapid_keys (Web Push key pair per schema) (12) | dropped | no push; not imported |
| 202 | push_subscriptions (12) | dropped | 0 rows |
| 203 | scan_state (watcher high-water mark) (12) | dropped | no scan |
| 204 | cc_shared.shares (share links) (12) | covered | 12 + 19.1 imported with same ids |
| 205 | Recently deleted purge (process-side and router-side) (12) | covered | 15 purge job over timestamptz |
| 206 | Session-list cross-process relay (LISTEN/NOTIFY) (12) | covered | 14 fan-out |
| 207 | Legacy old-UI tables kept with rows (users, conversations, messages, turns, events, claw_sessions, files, shares, blobs, blob_grants) (13) | covered | 19.3 not imported: archived, then deleted (decision 5, cutover 15.4) |
| 208 | Bucket prefix jarvis/files/<conversationId>/<sha256> (old UI uploads and agent files) (13) | covered | 19.3 archived, then deleted (decision 5, cutover 15.4) |
| 209 | Bucket prefix yyc-ai/ (JuiceFS data, live) (13) | covered | 19 transcripts/sent files imported into rows + bucket |
| 210 | ops_events view_as (current, never pruned) (13) | covered | 19.1 -> view_as_log |
| 211 | Legacy sandbox-reset note stripped from saved prompts (13) | covered | 19.3 stripped once in display data |
| 212 | superseded_provider_sessions table (13) | dropped | 0 rows; no superseded transcripts |
| 213 | Provisional conversation names (sessions.name_source NULL) (13) | covered | 9.4 + 19.1 name_source carried |
| 214 | Deleted = isArchived 1 + deleted_at, purge after 30 days; Archive removed (13) | covered | 9.6 deleted_at; archive gone |
| 215 | SQLite-compat date functions over mixed-format TEXT timestamps (13) | covered | 19.1 normalised to timestamptz |
| 216 | Disk-discovered sessions keyed by provider id (13) | covered | 19.1 kept with that id |
| 217 | jarvis_failed_turns (13) | covered | 19.1 imported as events |
| 218 | cc_shared.shares (current share feature) (13) | covered | 19.1 |
| 219 | Hub chat client bootstrapped from the first Paseo connection (14) | dropped | no hub; REST + events |
| 220 | View-as audit and limits on the Paseo socket (14) | covered | 13 viewer on events + view_as_log; create, send, cancel and upload recorded in the same transaction |
| 221 | Per-conversation SessionTimeline cache (14) | dropped | no in-memory projection; the rows table (7.3) is the projection |
| 222 | Timeline paging (tail/before/after, epoch reset) (14) | covered | 7.4 rows paging; epoch changes only on visible rewrites (7.3 rule 9) |
| 223 | Live event follow (chat.subscribe from the hub) (14) | covered | 7.4 subscribe-then-query long-poll by row seq |
| 224 | Send through the hub (optimistic user row + chat.send) (14) | covered | 8.2 write-then-ack |
| 225 | User-message text-match dedupe (14) | covered | 8.2 clientMessageId + 9.2 idempotencyKey; 6.5 echo hidden |
| 226 | Edits from the CloudCLI web UI not reflected (14) | dropped | edit dropped; retract covers transcript rewrites |
| 227 | Turn lifecycle events (started/completed/failed/canceled) (14) | covered | 8.1 turns with outcome events |
| 228 | Concurrent sends from two sockets (14) | covered | 8.2 single admission under row lock |
| 229 | Directory sync seq / generation (14) | covered | 7.6 change seq and a stable generation |
| 230 | Hub/timeline memory footprint vs child heap (14) | covered | 7.4 paged rows, blobs as references |
| 231 | Client frame validation: silent drop vs request failure (15) | covered | 7.1 additive shapes; unknown kinds skipped |
| 232 | Directory generation per process; always-snapshot sync (15) | covered | 7.6 durable cursors, stable generation |
| 233 | Timeline epoch per process; full transcript rebuild (15) | covered | 7 stable seq |
| 234 | Survival: ui pod drain longer than JARVIS_UI_DRAIN_SECONDS (900 s) (16) | covered | 18.3: turns live in the supervisor; streams resume by seq |
| 235 | Survival: CloudCLI child crash or V8 heap OOM (--max-old-space-size 512) (16) | covered | no per-person heap; history in rows (7) |
| 236 | Survival: router peer hand-off or rendezvous change while a WebSocket is open (16) | covered | 7.4 any replica answers polls; lease prevents split brain (6.9) |
| 237 | Survival: CNPG single instance restart (Postgres for CloudCLI, jarvis and JuiceFS metadata) (16) | covered | 6.13 503 + retries instead of log-and-drop |
| 238 | Survival: JuiceFS mount-pod restart on the UI node (16) | dropped | magpie mounts no JuiceFS |
| 239 | Survival: sbx-jfs restart on the sandbox host (juicefs crash) and EDQUOT (16) | covered | 6.10 ack only after store; /workspace health is hermit's |
| 240 | Stop (chat.abort) during a dead /proc link (16) | covered | 8.6 cancel recorded at once, forced after 30 s |
| 241 | jarvis_failed_turns: the only per-turn failure record (16) | covered | 8.2/8.9 every failure is a turn.ended event |
| 242 | Readable error mapping shown to staff (zh-HK) (16) | covered | 8.9 codes; pigeon presents |
| 243 | Soft delete into Recently deleted (17) | covered | 9.6 |
| 244 | Restore from Recently deleted (17) | covered | 9.6 |
| 245 | Recently deleted list with purgeAt (17) | covered | 9.1 purge_at, 9.3 |
| 246 | Force (permanent) delete (17) | covered | 9.6 purge removes the session folder, tool results, uploads and sent files |
| 247 | onConversationDeleted side effects (17) | covered | 9.6 + outbox |
| 248 | 30-day purge inside each user process (17) | covered | 15 single job |
| 249 | Router purgeWake (17) | covered | 15 purge by time |
| 250 | Hidden delete: synchronizer prunes rows whose transcript vanished (17) | covered | 6.8 transcript unlink refused; no prune |
| 251 | Share create (snapshot) (17) | covered | 12.1 (pointer + row copy, no workspace cp by magpie) |
| 252 | Share revoke (Stop sharing) and revoke-on-delete (17) | covered | 12.2 + 9.6 |
| 253 | Share continue (17) | covered | 12.4 |
| 254 | Uploads store (cloudcli-assets) never deleted (17) | covered | 10.1 GC of unattached; attached go with the conversation |
| 255 | Claude Code per-user leftovers in ~/.claude (17) | covered | 6.1 conversation files in magpie, purged with it; local rest dies with the sandbox |
| 256 | Sent-file snapshots (17) | covered | 11 |
| 257 | Offboarding: remove an email from config.json users (17) | covered | 9.6 person purge, called only by hermit's operator route; no automatic deletion |
| 258 | View-as and deletes (17) | covered | 13 refusals |
| 259 | JuiceFS trash and bucket objects (17) | covered | 5.3/18.6 explicit GC and deletion semantics |
| 260 | Volume format settings (18) | dropped | magpie uses no JuiceFS volume |
| 261 | No trash (TrashDays 0) (18) | covered | 18.6 deletion semantics |
| 262 | Per-conversation /workspace quota 1 GiB / 1,000 inodes (18) | not magpie | hermit (workspace quota); magpie's own quota 6.12 |
| 263 | Quota is missing on conversations whose first message had an attachment (live: 3 of 37) (18) | covered | 10.3 magpie never creates workspace folders |
| 264 | No quota on users/<key>/claude, shares or sent (18) | covered | 6.12 2 GiB per conversation; shares share blobs |
| 265 | Sent-file snapshots via juicefs clone (18) | covered | 11.2 content-addressed blobs (21.1) |
| 266 | cloneTree for sent/ on fork, share create, continue-share (18) | covered | 12 shares/continue copy references |
| 267 | Full (non-CoW) copies of /workspace on fork and share (18) | covered | 12 share freeze and continue are hermit workspace clones (metadata-only in YYC) |
| 268 | Deletion of a conversation's files (18) | covered | 9.6 outbox retries |
| 269 | Files panel reads /jfs/ws and sent snapshots (18) | covered | 11.1/11.3 |
| 270 | Automatic metadata backup (hourly meta dump to the bucket) (18) | not magpie | hermit keeps the JuiceFS metadata backups (/workspace is hermit's; 21.2 lists what hermit must own) |
| 271 | Postgres backups of jfs_yyc_ai (18) | not magpie | ops; magpie DB added to dumps (18.2) |
| 272 | No gc / fsck jobs (18) | covered | 15 blob GC + reconcile |
| 273 | Bucket yyc-ai-files (18) | covered | 5.2 own bucket yyc-ai-magpie |
| 274 | AWS host sbx-jfs.service (18) | not magpie | hermit host storage |
| 275 | UI HOME/.claude symlink to users/<key>/claude (18) | dropped | no filesystem in the UI |
| 276 | Live dashboard: YYC AI operator snapshot poll (live_once) (19) | covered | 16.4 /v1/ops/people for hermit's snapshot |
| 277 | Snapshot fields read by ai_slim (state contract) (19) | covered | 16.4 /v1/ops/people fields hermit maps to ai_slim |
| 278 | Daily locked pg_dump of YYC AI's databases (19) | covered | 18.2 |
| 279 | YYC AI databases and roles declared in the db namespace (19) | covered | 18.2 |
| 280 | Postgres network access for YYC AI pods and sandbox hosts (19) | covered | 18.2 magpie needs Postgres only from the cluster |
| 281 | Router test suite (Access, per-person process, rendezvous, rollouts, drain, view-as, ops proxy, sign-in, sent-file snapshot) (20) | covered | 20 sent-snapshot tests; no-lost-stream rollout test |
| 282 | Jarvis-mode server tests (fork server in Jarvis mode) (20) | covered | 20 ported cases |
| 283 | Shares tests (20) | covered | 20 |
| 284 | Title generation tests (20) | covered | 20 |
| 285 | Sandbox-preparing indicator tests (20) | covered | 20 cancel while preparing, preparing events |
| 286 | Image attachment tests (20) | covered | 20 |
| 287 | History note and local-mode tests (20) | covered | 20 + 19.3 |
| 288 | Paseo bridge tests (20) | covered | 20 paging tests against pigeon over magpie |
| 289 | Core CloudCLI server tests YYC relies on (npm test, not in any CI) (20) | covered | 20 |
| 290 | Local end-to-end harness (no cloud, no model cost) (20) | covered | 20 fake hermit + mock Anthropic |
| 291 | ui-pod-chat (real chat from inside a ui pod) (20) | covered | 21 scripts rewritten against pigeon/magpie |
| 292 | View-as checks (pod and browser) (20) | covered | 20 |
| 293 | Share viewer check (20) | covered | 20 |
| 294 | Measured baselines (acceptance numbers) (20) | covered | 20 baselines (title, share latency) |
| 295 | Zero-downtime rollout configuration verified by tests (20) | covered | 18.3 + 20 rollout test |

Area numbers are indices into `inventory.json` `areas`.

---

## 23. Review round 1: where each point landed

| # | Point (severity) | Resolution in this spec |
|---|---|---|
| 1 | JuiceFS A18 mechanics unowned; row 270's reason wrong (minor) | Row 270 now "not magpie: hermit keeps the JuiceFS metadata backups"; 18.6 says `/workspace` backups are hermit's; 21.2 lists the A18.F0, F5, F11, F13, F16-F19 dispositions hermit must write (hermit's own table is hermit's file) |
| 2 | No owner of the Paseo timeline rows (blocker) | magpie owns them: 7.3 (`rows`, written in the event's transaction under the conversation lock, dense `rowSeq`, stable epoch), 7.4 (M9/M10), 12.3 (share reads return rows); pigeon keeps only the Row → PaseoRow mapping; platform R27 (21.3); cutover 18.2 to point here (21.2) |
| 3 | magpie 16.1 differs from pigeon 14.1 in meaning (blocker) | 16.1 is now pigeon 14.1 M1-M17 under `/v1/`, camelCase: M4 added (9.2); M12 change types (7.6, same transaction as the event); long-poll `waitMs`, no NDJSON (7.4, 7.6); M5 `PATCH`; M6 `POST …/delete` and `DELETE` with 409 `not_deleted`; M8 conversation-level cancel incl. `stop_tasks` (8.6); turn bodies `images:[{uploadId}]` + `files` (8.2); M13 headers and response (10.1); M15 `/v1/blobs/{blobId}?variant=` (5.4); M16 rows inline and 409 `nothing_to_share` (12); M17 `/v1/view-as` (13); view-as audit in the action's transaction (13); pigeon's Conversation object plus `migrating` (9.1); pigeon's side listed in 21.2 |
| 4 | Sandbox calls don't match hermit 3.6 (blocker) | 8.3 uses `PUT …/sandbox {person}`, 200/202, 403 `not_allowed`, 409 `person_mismatch` → `internal`, 503 `sandbox_busy {reason, retryAfterS}` → `sandbox.busy`; release through `POST …/sandbox/release {reason}` (15 outbox); `waitMs/afterVersion` long-poll; the R8 pull feed (8.7); `sandbox.budget` removed (budget is `model.budget` from egress, 8.9) |
| 5 | Workspace API doesn't exist in hermit (blocker) | Shares are hermit workspace clones `uuidv5(SHARE_NS, "share:"+id)` (12); continue clones from it; revoke and purge `DELETE /v1/workspaces/{wid}`; live reads `GET /v1/workspaces/{id}/files` (11.3); importer convention as hermit/cutover (19.1); consumes list 21.2 |
| 6 | Supervisor uses routes and fields magpie lacks (blocker) | Added: `/live` (7.5), `stop_tasks` (8.4, 8.6), `run.oom` (8.9), task event `id` (8.5, `report_ids`), heartbeat `gen/supervisor/cc` and `lastTurn` (8.5), `usage`/`durationMs` stored (8.5), `asks/{id}/resolved {outcome}` (8.5), tree `ccBytes`/`quotaBytes` (6.11), `kill {reason}` (8.4); pigeon's consumer of live frames is M9 `live` (7.5, 21.2) |
| 7 | Two owners stage uploads (major) | The supervisor writes uploads; magpie never writes the workspace (8.4, 10.3); hermit's staging sentence to go (21.2) |
| 8 | Purge route has no caller; `exists` undefined (major) | 9.6: hermit's operator route `POST /v1/admin/people/{email}/purge` (person not in the catalogue) forwards to magpie, which re-checks hermit; no automatic deletion. 9.7: `POST /v1/hermit/workspaces/exists` replaces hermit's `GET /conversations/exists` |
| 9 | Pre-reconciliation credentials, headers, ports, case (minor) | Applied in text: `MAGPIE_ACCEPT_*`/`MAGPIE_TOKEN_HERMIT` and `YYC_TOKEN_*` store keys (3.1, 18.2), `Aviary-*` headers (3), 8080/9090 and Service port 80 (16.5, 18.2), camelCase bodies and lower_snake_case codes throughout, namespace `yyc-ai`, R11 pool, R15 subcommands, R17 CI, R19 ops paths |
| 10 | Three mechanisms and a 2 s poll for sandbox state (minor) | 8.3 long-poll for start progress; 8.7 feed for lost/released; 8.8 asks hermit only on heartbeat silence; no fixed poll |
| 11 | Streaming contract disagrees (blocker) | Same as 2 and 3: rows with dense `rowSeq`, a tool row re-issued at the next seq on its result (7.3 rule 5), `after` paging that yields every seq once (7.4), epoch bumped when a retract changes visible rows (7.3 rule 9), M9 `waitMs` over LISTEN, M12 with `turn {phase, timelineSeq}`, `sandbox`, `ask`, `ask_resolved`, `timeline_replaced`, all in the event's transaction (7.6); contract test and the "reconnect mid-turn across a compaction" scenario (20) |
| 12 | Live frames have no magpie route or consumer (major) | 7.5: unlogged `live_frames` idempotent by `(turnId, n)`, cleanup 60 s after the turn, `live` in M9 with `liveAfter`, `GET …/live` for a mid-message join, supersede by `(messageId, blockIndex)` in the durable row's transaction; 409 `turn_not_active` never retried; pigeon mapping in 21.2 |
| 13 | Watchdog kills healthy turns after a magpie outage (major) | 8.8: silence measured from `greatest(heartbeat_at, available_since)`; `available_since` reset by the every-replica tick after any 20 s gap (not the leader alone, which would miss a gap during a leader handover); before failing as unreachable magpie asks hermit, whose view does not depend on magpie |
| 14 | Dropped end reports leave turns running for ever (major) | Both fixes: reports are never dropped (8.5, 21.2), and every heartbeat reconciles `turnId`/`state`/`lastTurn` against the active turn (8.5 rules 1-4) |
| 15 | Retry forever vs EIO after 10 min (major) | One rule (6.12, R26): unbounded under general unavailability; one op stuck 10 min while the heartbeat succeeds → `cc.write_stuck`, generation stopped, `cc_resync` so the next lease re-uploads the cached suffix |
| 16 | Lease granted unconditionally (minor) | 6.9: only to hermit's current sandbox (feed, else a direct hermit read), else 409 `not_current_sandbox`, and the supervisor stays unready without a restart loop |
| 17 | Late lines credited to the wrong turn (minor) | `turnId` on every cc write (6.3, 16.2); end reports carry `lastUuid` and get 409 `transcript_pending` until it has landed (8.5); the supervisor drops its 5 s cap (21.2) |
| 18 | One `deadline` for every state (minor) | 8.8 table: per-state deadlines; a running turn's deadline moves with each heartbeat; no total limit on a running turn |
| 19 | Cancel, PUT, control token, origin mismatches (minor) | M8 cancel (8.6); hermit `PUT` (8.3); magpie never calls :7070, so hermit should drop the token from the Sandbox object (1, 21.2); origin `person`/`agent`/`import` (4, R30) |
| 20 | Two contracts for pigeon's read path (blocker) | One contract: magpie's rows (decision 3). Taken from this review: long-poll with `waitMs` instead of NDJSON (drains stay <= 25 s), one change-feed name `/v1/changes`, the zero-refetch contract test over interleaved non-row events (20). Not taken: pigeon projecting events (section 24) |
| 21 | Live streaming only specified on the supervisor side; Paseo merge risk (major) | 7.5 on magpie's side; 21.2 requires pigeon's replace-not-merge mapping and a contract test that runs the app's real reducer; the supervisor's `--include-partial-messages` ships only once that test is green |
| 22 | Acknowledged writes wait for bucket PUTs (major) | 5.2: new blob bytes are staged in Postgres in the write's transaction and uploaded afterwards; reads serve from staging; acceptance p99 <= 150 ms for a 1 MiB image line (6.10, 20) |
| 23 | Hot path on a single-instance shared Postgres (major) | 18.2: own Cluster `magpie-pg`, 3 instances, synchronous `any 1` with `dataDurability: required`, switchover updates; the spend (EUR 2.06 per month) is open question 21.4.1, with the explicit fallback stated; the offset mismatch is a repair path (6.12) |
| 24 | `main_transcript` bound to file identity breaks on set-aside (major) | 6.2: the main transcript is a path; rename away keeps old events but makes no new ones; a fresh file at the path matches moved lines by sha; Claude Code release gate in CI (20) |
| 25 | Limbo flushed mid-rewrite (minor) | 6.7: debounced on 10 s of no operation on the file, or the turn's end; never while a rewrite progresses |
| 26 | Three unreconciled contradictions (minor) | R26 retry rule (6.12), R29 `link` → EPERM (6.8), R28 snapshot stays 501 and shares use clone only (12); rows proposed in 21.3 |
| 27 | Auto-memory per person silently lost (minor) | 6.1 and 19.1: nothing is stored there today (A8.F18) and the importer's plan fails if that changes; the behaviour change is open question 21.4.2 |

## 24. Rejected review points

1. **Point 20's recommendation that magpie serve only events and pigeon project them into Paseo rows**, folding
   non-row events into the next row's `sourceSeqRanges` (or sending seq-carrying no-op rows). Not taken, because:
   - pigeon is stateless with more than one replica (target design), and the projection is stateful: a tool row
     changes when its result, a task notification, a task status or a sent-file snapshot arrives (possibly hours later
     for a background task), a turn's end cancels its running tool rows, and a retract must find and rebuild the rows
     it touched. Doing that in pigeon means every replica re-deriving state from an unbounded window of events on
     every poll, or keeping the state, which pigeon must not.
   - Folding a non-row event into the *next* row stalls the live stream: after `turn.ended` or `task.status` there may
     be no next row for hours, so the app's cursor cannot advance past that event seq and every later poll looks like a
     gap. No-op rows are not a Paseo item type; the app would have to be taught them (the shipped iOS builds cannot
     be).
   - Retraction cannot reach the app through a pure per-event mapping: a retract names event seqs, and only the
     holder of the row model knows which visible rows they were.
   The parts of point 20 that do not depend on this were adopted (section 23, row 20). The target design's "pigeon
   streams `GET /events?after=<seq>`" is met by the row stream: it is per conversation, ordered by seq, resumes by seq
   and pages `before=<seq>&limit=40` at page-size cost; the events themselves stay in magpie as the record.
2. **Point 11's "epoch bumped by a retract or rename-over"**, taken only in part: the epoch changes when a retract or
   rename-over actually removes or rebuilds a visible row (7.3 rule 9). Claude Code renames over its transcript at
   every compaction, almost always keeping every visible line; bumping the epoch then would make every open tab drop
   its cache and repaint after each compaction for no change.
3. **Point 27's premise that existing auto-memory is "silently dropped"**: no person has any file under
   `projects/-workspace/memory/` (inventory A8.F18), so the import loses nothing; the importer now fails
   its plan rather than skip a file if one appears before the switch. The behaviour change itself was put to Deyao
   (21.4.2), as point 27 asked, and he chose per conversation (decision 6); a file that appears is now copied into
   each of the person's conversations.
4. **Point 13's "set by the leader when the DB com