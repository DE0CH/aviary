-- Limbo holds every classified main-transcript line a truncate or a move takes off the main path, not only lines
-- with events (magpie 6.6 step 5, 6.7): a line classified as internal (the echo of a person's prompt, an attachment)
-- that is written again matches its entry and is not classified a second time, so a rewrite, a compaction or a
-- set-aside never turns an echo into a new prompt. Entries without an event are dropped at a flush (nothing to
-- retract). Each entry keeps the turn its line was credited to (6.3) and the turn of the operation that put it there,
-- so the end of a turn flushes only the limbo that turn's operations made.
alter table cc_limbo drop constraint cc_limbo_pkey;
alter table cc_limbo alter column event_seq drop not null;
alter table cc_limbo add column id bigserial primary key;
alter table cc_limbo add column turn_id uuid;
alter table cc_limbo add column op_turn uuid;
create unique index cc_limbo_event on cc_limbo (scope, reason, event_seq) where event_seq is not null;
-- an end report's flush (magpie 6.7)
create index cc_limbo_op_turn on cc_limbo (scope, op_turn) where reason = 'truncated';
