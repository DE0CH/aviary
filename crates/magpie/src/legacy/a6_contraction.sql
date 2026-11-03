-- A6 (cutover 10.5 step 7): the window's tables and columns go; the legacy-path set (import_uploads,
-- uploads.legacy_path) stays until A7.
drop table legacy_tickets;
drop table import_sources;
drop table import_shares;
drop table import_control;
drop table import_purged;
alter table conversations drop column legacy_fence_at;
alter table conversations drop column migrating;
