-- legacy_shares.deleted_at: when hermit deleted an old share's workspace through its API (magpie revoking the share
-- or purging its conversation: DELETE /v1/workspaces/<id>) or legacy-cleanup removed it. `hermit import-jarvis` never
-- clones such a share again (the old share table still holds it live: only the old UI wrote it), and
-- `hermit legacy-cleanup` counts it carried over (hermit 22.3 steps 3 and 6). Dropped with legacy_shares.
alter table legacy_shares add column deleted_at timestamptz;
create index legacy_shares_workspace on legacy_shares (workspace);
