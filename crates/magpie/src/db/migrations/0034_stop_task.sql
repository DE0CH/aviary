-- The supervisor's command for stopping one task (the app's activity panel, src/turns/tasks.rs).
alter table commands drop constraint commands_type_check;
alter table commands add constraint commands_type_check
  check (type in ('start_turn', 'interrupt', 'kill', 'title', 'answer_ask', 'stop_tasks', 'send_message', 'stop_task'));
