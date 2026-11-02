-- The view-as audit is read newest first by (at, id) and paged by an entry's (at, id) (magpie 13).
create index view_as_log_at on view_as_log (at desc, id desc);
