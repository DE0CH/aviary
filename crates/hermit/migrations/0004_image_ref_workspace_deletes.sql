-- The image a start creates is fixed when its row is inserted: `image` (the digest) and `image_ref` (the pinned
-- `<repo>@sha256:…`) are the inserting replica's configured image. Whichever replica drives the start creates that
-- image, and a recycle compares the row's own value, so a start inserted by an old replica during a rollout comes up
-- on the old image and is recycled like any other sandbox on it.
alter table sandboxes add column image_ref text;

-- workspace_deletes: a `DELETE /v1/workspaces/{wid}` in flight (written before it releases the sandbox, removed with
-- the tree). While a row is younger than hermit's delete guard (10 min), a start's workspace step and
-- `PUT /v1/workspaces/{wid}` refuse the workspace, so no sandbox mounts a tree being removed. An older row (a replica
-- that died mid-delete) is ignored and replaced by the retried delete.
create table workspace_deletes (
  id uuid primary key,
  started_at timestamptz not null default now()
);
