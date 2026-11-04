-- import_sources.released_checked: the stats of a released conversation's source files at which the hand-over loop
-- last ran its released check (DESIGN 9.9 import_verify_mismatch_total, counted live): a difference from the release's
-- record is checked, and its mismatches counted, once across every process that leads the loop, so a restart or a
-- new leader does not count a write again. An expand step, dropped with the import tables in A6.
alter table import_sources add column released_checked jsonb;
