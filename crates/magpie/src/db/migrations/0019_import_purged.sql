-- Conversations deleted for good on the new side during the switch window (magpie's purge: M6 delete for good, the
-- 30-day purge, a person's purge; not the importer's forget). The reverse export (cutover 14.3: "deletions for good on
-- the new side delete the row and the files") reads them to delete the conversation's sessions row, transcript files,
-- failed turns, sent files and share links on the old side, which it cannot find once the conversation's rows are gone.
-- Expand-only; dropped with the window's other tables in A6.
create table import_purged (
  conversation_id     uuid primary key,
  person              text not null,
  provider_session_id text,                     -- Claude Code's session id (the transcript's file name)
  cc_paths            text[] not null,          -- the conversation's cc files and folders at the purge
  purged_at           timestamptz not null default now()
);
create index import_purged_at on import_purged (purged_at);
