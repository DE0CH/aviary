-- aviary-pg: no-transaction
-- The admin reads' audit (magpie 13) looks up today's `admin_read` row of an admin and conversation before writing
-- one (`viewas::admin_reads`, under a per-admin transaction lock). Not unique: rows already written stay, and a
-- replica still running older code inserts without a conflict clause.
create index concurrently if not exists view_as_admin_read on view_as_log (admin, conversation_id, at)
  where event = 'admin_read'
