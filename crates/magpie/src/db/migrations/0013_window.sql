-- The switch window's tables and columns (DESIGN 9.2-9.5, cutover 6.4 and 7.8, switch-final 4.3 and 23 C5), expand
-- only. `legacy_tickets`, `conversations.legacy_fence_at` and `import_control.rollback_at` go in A6 with
-- `migrating` and the import tables; the legacy-path set (`import_uploads`, `uploads.legacy_path`) stays until A7, so
-- nothing in it references a table the A6 contraction drops (`crate::legacy::A6_CONTRACTION`, tested).

-- The fence (cutover 7.8): set by the importer's fence transaction, cleared by its release or unfence. A fence is a
-- 3 s lease (`limits::FENCE_LEASE`): older than that it no longer holds, so a dead importer leader never keeps a
-- conversation's writes waiting.
alter table conversations add column legacy_fence_at timestamptz;

-- A write pigeon sends through its legacy upstream takes a ticket first; only an outcome closes it (switch-final C5.1).
create table legacy_tickets (
  ticket             uuid primary key,
  conversation_id    uuid not null references conversations on delete cascade,
  kind               text not null check (kind in ('send','stop','rename','delete','restore','purge','warm',
                                                  'shareCreate','shareRevoke','upload')),
  client_message_id  text,
  issued_at          timestamptz not null default now(),
  delivered_at       timestamptz,               -- the write reached the old child (recorded, closes nothing)
  done_at            timestamptz,               -- closed on an outcome
  outcome            text check (outcome in ('started','completed','failed','applied','refused')),
  check ((done_at is null) = (outcome is null))
);
create index legacy_tickets_open on legacy_tickets (conversation_id) where done_at is null;
create index legacy_tickets_conversation on legacy_tickets (conversation_id, done_at);

-- `cutover rollback flip` (DESIGN 9.10 step 1): set while creates go through the legacy upstream.
alter table import_control add column rollback_at timestamptz;

-- The legacy-path set (DESIGN 9.5), until A7. An old asset path is held in its canonical form: the file name inside
-- the person's old asset folder (`magpie_api::legacy::legacy_asset_name`), so the old home's root never matters.
-- `uploads.legacy_path`: the T0->gate upload tee recorded these bytes under that old path.
alter table uploads add column legacy_path text;
create index uploads_legacy_path on uploads (person, legacy_path, created_at desc) where legacy_path is not null;

-- The importer's ingestion of every person's old asset folder (from A2, every 5 s, once more inside each release,
-- and synchronously on a send's miss). `upload_id` is an unattached upload holding the bytes; each send attaches a
-- fresh copy of it, since the old stack reuses an asset path any number of times.
create table import_uploads (
  person       text not null references persons on delete cascade,
  legacy_path  text not null,
  upload_id    uuid not null references uploads on delete cascade,
  sha256       bytea not null,
  imported_at  timestamptz not null default now(),
  primary key (person, legacy_path)
);
create index import_uploads_upload on import_uploads (upload_id);
