-- The per-person gate (cutover 7.9 `allow-handover --person <email>`, DESIGN 9.9 rehearsal R1): the importer
-- releases that person's conversations once their part of every ui report is clean, before the gate for everyone
-- (`import_control.handover_allowed_at`) is set; `GET /v1/legacy/window` answers the person's own gate as theirs. An
-- expand step: one table, dropped with the import tables in A6.
create table import_gate_persons (
  person      text primary key,
  allowed_at  timestamptz not null default now()
);
