-- legacy_share_deletes: every workspace hermit deleted on purpose through its API (`DELETE /v1/workspaces/<id>`:
-- magpie revoking a share or purging its conversation) or `hermit legacy-cleanup` removed, by workspace id, written
-- before the delete removes anything. It replaces `legacy_shares.deleted_at`, which an UPDATE could only set on a
-- share row the import had already read: a delete that came first (a share magpie imported before hermit's share pass
-- saw its row) left no trace, and the next pass cloned the folder back. The id alone cannot tell an old share's clone
-- from any other workspace, so every delete is recorded (a few rows a day for the window). `hermit import-jarvis` never
-- clones a share whose workspace is here, and `hermit legacy-cleanup` counts it carried over (hermit 22.3 steps 3 and
-- 6). Dropped with legacy_shares.
create table legacy_share_deletes (
  workspace  uuid primary key,
  deleted_at timestamptz not null default now()
);
insert into legacy_share_deletes (workspace, deleted_at)
  select workspace, min(deleted_at) from legacy_shares where deleted_at is not null group by workspace;
alter table legacy_shares drop column deleted_at;
