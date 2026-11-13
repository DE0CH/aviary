-- A message with attachments sent while a turn runs joins that turn too (src/turns/midturn.rs): `send_attached`
-- carries its files to the supervisor, which writes them into the workspace and the message into the turn.
--   placed  the `send_attached` payload the message was written with (its files' workspace paths); null until then
--   hold    the message could not join the running turn (a supervisor that does not know `send_attached`, or a
--           file it could not write): it waits for the turn's end and starts a turn of its own
alter table commands drop constraint commands_type_check;
alter table commands add constraint commands_type_check
  check (type in ('start_turn', 'interrupt', 'kill', 'title', 'answer_ask', 'stop_tasks', 'send_message', 'stop_task',
                  'send_attached'));
alter table pending_messages add column placed jsonb;
alter table pending_messages add column hold boolean not null default false;
