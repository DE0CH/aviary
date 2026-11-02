-- The hourly retention pass (magpie 15, 17) deletes by age; these let it find the old rows without a full scan.
create index changes_at       on changes (at);
create index cc_ops_at        on cc_ops (at);
create index report_ids_at    on report_ids (at);
create index commands_acked   on commands (acked_at) where acked_at is not null;
create index uploads_unattached on uploads (created_at) where conversation_id is null;
