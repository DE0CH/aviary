-- WP-13 review fixes. Expand-only; like 0005 its indexes are created without CONCURRENTLY, on the database the
-- importer then fills (magpie applies every migration at start, before the import).

-- The derivation version (rows::DERIVATION) a row was made or last updated by: a release that changes the
-- projection's rules rebuilds the rows below its version from their events.
alter table rows add column derivation smallint not null default 1;

-- `retracted` also holds the seq of a line a rewritten line (`meta.replaces`) took over before its retract landed:
-- it no longer counts for block indexes or rebuilds.

-- A rewritten assistant line stands where the line it replaces stood (rule 4's block index): `pos` is the seq of
-- the first line of that chain of rewrites. A line without a row here stands at its own seq.
create table line_pos (
  conversation_id uuid not null references conversations on delete cascade,
  seq             bigint not null,
  pos             bigint not null,
  primary key (conversation_id, seq)
);

-- A live delta repeating stored bytes (magpie 7.5) is found by its block and offset, not by scanning the turn.
create index live_frames_delta on live_frames (turn_id, message_id, block_index, byte_offset) where op = 'delta';
