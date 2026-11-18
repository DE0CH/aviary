-- Rule 11 (rows::DERIVATION 3): a question of the `ask` command and a command's wait on the person are tool rows
-- with a card (`item.person`); every tool result looks for the answers the AI has not read yet. This index keeps that
-- lookup to the few rows that can be read; a row made before has no card, so nothing is rebuilt.
create index rows_person_unread on rows (conversation_id, row_id)
  where kind = 'tool' and item ? 'person' and (item->'person'->>'state') <> 'open' and (item->'person'->'readBy') is null;
