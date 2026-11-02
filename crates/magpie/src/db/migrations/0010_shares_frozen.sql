-- WP-15 review (magpie 12.4, 9.7, 13). Expand-only.

-- The event that recorded a retraction (a `retract` event, or the line that replaced the retracted one), so a
-- continue copies only the retractions its share's events made. Null for rows recorded before this column (and by
-- writers that do not set it): they count as made by the retracted event itself.
alter table retracted add column by_seq bigint;
alter table cc_retracted add column by_seq bigint;

-- The conversation's model at the freeze (a continue takes it from the share, not from the source).
alter table shares add column model text;

-- hermit workspaces cloned for a share or a continued conversation whose row has not committed yet: reported by
-- `POST /v1/hermit/workspaces/exists` so hermit's cleanup never removes a clone about to be referenced. The
-- creating transaction deletes its row; retention removes rows a replica left behind (and queues their clone's
-- deletion).
create table pending_workspaces (
  workspace_id uuid primary key,
  created_at   timestamptz not null default now()
);
