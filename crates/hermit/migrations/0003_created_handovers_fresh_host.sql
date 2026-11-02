-- fresh_host_set_at: when POST /v1/admin/fresh-host was last called (hermit 16); placement's baseline, written by
-- nothing else (controls.updated_at also moves with the sandboxes-enabled switch).
alter table controls add column fresh_host_set_at timestamptz;
update controls set fresh_host_set_at = updated_at where fresh_host_until is not null;

-- handovers: how many times a start was handed to another replica at its workspace step (15.6); bounded so two
-- replicas whose mounts flap never pass a start back and forth for ever.
alter table sandboxes add column handovers int not null default 0;

-- A workspace's creation time (hermit-api Workspace.createdAt), written when ensure makes the folder or a clone
-- makes the copy; a workspace made before this table gets its row on first read. JuiceFS keeps no birth time and a
-- folder's mtime moves with every entry added to or removed from it. Removed with the workspace.
create table workspaces (
  id uuid primary key,
  created_at timestamptz not null
);

-- replicas rows are kept 1 day (the janitor's retention): pod names change on every rollout.
create index replicas_seen on replicas (seen_at);
