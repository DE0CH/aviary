-- The timeline projection's lookups (magpie 7.3-7.5). Expand-only.

-- Every row seq a row took, ascending (Row.seqs / sourceSeqRanges), so a page reads a row from one tuple.
alter table rows add column seqs bigint[] not null default '{}';
-- A model row by (messageId, blockIndex): live-frame supersede and late frames of a block that is already durable.
create index rows_block   on rows (conversation_id, message_id, block_index) where message_id is not null;
-- A row by its identifying event (search maps event seqs to rows).
create index rows_source  on rows (conversation_id, (source_seqs[1]));
-- The running tool rows of a turn (rule 8 at turn.ended).
create index rows_running on rows (conversation_id, turn_id) where kind = 'tool' and (item->>'status') = 'running';

-- Earlier assistant lines of the same API message (rule 4's block index).
create index events_message on events (conversation_id, ((data->'message'->>'id')), seq) where kind = 'cc.assistant';
-- A sent-file snapshot by its tool call (rule 4 fills sentFiles of a row made after the snapshot).
create index events_sent    on events (conversation_id, ((data->>'toolUseId')), seq) where kind = 'files.sent';

-- Event seqs a `retract` removed from the main transcript (rule 9). Events stay immutable; this is the projection's
-- record of which of them no longer count (block indexes, rebuilds).
create table retracted (
  conversation_id uuid not null references conversations on delete cascade,
  seq             bigint not null,
  primary key (conversation_id, seq)
);

-- `row_head` when a live block started (LiveBlock.afterRowSeq).
alter table live_frames add column after_row_seq bigint;
