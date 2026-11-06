-- Schedules (Deyao, 2026-11-06; src/schedules): a prompt the conversation's AI arms to be posted into that same
-- conversation once (`at`) or on a repeating rule (`cron` read in `tz`, until `until`).
--   id            'sch_' and 32 hex characters (the AI's handle for it)
--   describe      the rule in plain Hong Kong Chinese, as the server read it (the AI repeats it to the person)
--   next_at       the next fire; null when paused, done or deleted
--   state         active | paused | done (a one-off that fired, a rule past its end) | deleted (kept for the runs)
-- schedule_runs: one row per due time, the key that makes a fire happen once (src/schedules/fire.rs):
--   outcome       sent | pending | sent_mid_turn | redeliver | skipped_late | skipped_paused | skipped_deleted | failed
--   turn_id       the turn it started (sent) or joined (pending, sent_mid_turn)
--   cmd_seq       the send_message command of a mid-turn delivery
-- schedule_settings: the admin's pause-all switch (one row).
create table schedules (
  id               text primary key,
  person           text not null,
  conversation_id  uuid not null references conversations on delete cascade,
  kind             text not null check (kind in ('once', 'cron')),
  prompt           text not null,
  at               timestamptz,
  cron             text,
  tz               text not null,
  until            timestamptz,
  describe         text not null,
  next_at          timestamptz,
  state            text not null check (state in ('active', 'paused', 'done', 'deleted')),
  created_at       timestamptz not null default now(),
  created_by       text not null,
  updated_at       timestamptz not null default now(),
  deleted_at       timestamptz,
  check ((kind = 'once') = (at is not null) and (kind = 'cron') = (cron is not null))
);
create index schedules_due on schedules (next_at) where state = 'active';
create index schedules_person on schedules (person) where state <> 'deleted';
create index schedules_conversation on schedules (conversation_id);

create table schedule_runs (
  schedule_id      text not null references schedules on delete cascade,
  due_at           timestamptz not null,
  at               timestamptz not null default now(),
  outcome          text not null check (outcome in ('sent', 'pending', 'sent_mid_turn', 'redeliver', 'skipped_late',
                                                    'skipped_paused', 'skipped_deleted', 'failed')),
  conversation_id  uuid not null,
  turn_id          uuid,
  cmd_seq          bigint,
  detail           text,
  primary key (schedule_id, due_at)
);
create index schedule_runs_open on schedule_runs (conversation_id, cmd_seq) where outcome in ('pending', 'redeliver');

create table schedule_settings (
  id          int primary key check (id = 1),
  paused_all  boolean not null default false,
  paused_by   text,
  paused_at   timestamptz
);
insert into schedule_settings (id) values (1);

-- the supervisor's command for a mid-turn delivery
alter table commands drop constraint commands_type_check;
alter table commands add constraint commands_type_check
  check (type in ('start_turn', 'interrupt', 'kill', 'title', 'answer_ask', 'stop_tasks', 'send_message'));
