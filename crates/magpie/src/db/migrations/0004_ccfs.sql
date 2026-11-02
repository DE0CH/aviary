-- The Claude Code state store (magpie 6, WP-12): what the cc write path looks up on every operation.

-- A text blob's preview and line count are part of every event that references it (magpie 5.5, 6.5); kept with the
-- blob so an event can be made for a stored line (a promotion, a rename onto the main path) without its bytes.
alter table blobs add column preview text;
alter table blobs add column preview_truncated boolean;
alter table blobs add column lines bigint;

-- limbo is matched by sha256 within a scope (magpie 6.6 step 5, 6.2)
create index cc_limbo_sha on cc_limbo (scope, sha256);
-- the sweeper finds quiet files with truncated limbo (magpie 6.7)
create index cc_limbo_file on cc_limbo (file_id) where reason = 'truncated';

-- The uuids of retracted transcript lines, so a new line with the same uuid finds the event it replaces
-- (`meta.replaces`, magpie 6.7) by index rather than by scanning the conversation's events.
create table cc_retracted (
  conversation_id uuid not null references conversations on delete cascade,
  uuid            text not null,
  seq             bigint not null,
  primary key (conversation_id, uuid, seq)
);

-- Whether a line has been through classification (magpie 6.5). Lines written at a path before it is the main
-- transcript are not (false); promoting the file classifies exactly those, never a line already classified as
-- internal (an echo, an attachment). Lines another writer stores (the importer) default to classified.
alter table cc_lines add column classified boolean not null default true;
