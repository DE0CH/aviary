-- The model a person picks for a conversation (M5 `model`, or the deployment's at the conversation's creation): its
-- turns run on it from the next one (`start_turn.model`). Null: the deployment's default model.
alter table conversations add column picked_model text;

-- A person's messages sent while a turn ran (M7 `midTurn`, src/turns/midturn.rs): each is written into the running
-- turn (`send_message`), where Claude Code takes it in at its next tool result or when the model stops.
--   state    pending    the command is not acknowledged yet
--            written    the supervisor wrote it to Claude Code's stdin
--            taken      Claude Code took it in: it shows on the timeline (the transcript's prompt line with its
--                       text, as a user row with `client_message_id` for its id)
--            redeliver  its turn ended before it was written, or it was lost with its Claude Code: sent again
--            turn       sent again as a turn of its own (`turn_id`)
--            dropped    the conversation was deleted before it could be sent again
--   turn_id  the turn it joined; then the turn it became
create table pending_messages (
  conversation_id    uuid not null references conversations on delete cascade,
  client_message_id  text not null,
  text               text not null,
  state              text not null check (state in ('pending', 'written', 'taken', 'redeliver', 'turn', 'dropped')),
  turn_id            uuid,
  cmd_seq            bigint,
  created_at         timestamptz not null default now(),
  at                 timestamptz not null default now(),
  primary key (conversation_id, client_message_id)
);
create index pending_messages_open on pending_messages (conversation_id, created_at)
  where state in ('pending', 'written', 'redeliver');
