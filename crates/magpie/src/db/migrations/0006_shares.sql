-- Shares (magpie 12, WP-15). Expand-only.

-- A share's frozen rows keep every column of the rows they copy, so a continue makes rows the projection can go on
-- updating (a tool row found by its call id, a block by its message id, a row rebuilt from its source events) and
-- a share's Row carries its seqs like a timeline's.
alter table share_rows add column seqs        bigint[] not null default '{}';
alter table share_rows add column source_seqs bigint[] not null default '{}';
alter table share_rows add column tool_use_id text;
alter table share_rows add column message_id  text;
alter table share_rows add column block_index integer;
alter table share_rows add column is_prompt   boolean not null default false;

