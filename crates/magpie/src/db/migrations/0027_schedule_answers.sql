-- A schedule's run once its fired message is answered (Deyao, 2026-11-06: the panel's 已回覆, src/schedules):
--   answered           a `sent` run whose turn completed
--   answered_mid_turn  a `sent_mid_turn` run whose turn completed
--   unanswered         a `sent` or `sent_mid_turn` run whose turn failed or was canceled
-- magpie derives these when it reads a run from its turn (`turn_id`); only rows carried over from the old stack
-- (magpie-import, whose process recorded them when the turn's result came; no turn here) store them.
alter table schedule_runs drop constraint schedule_runs_outcome_check;
alter table schedule_runs add constraint schedule_runs_outcome_check
  check (outcome in ('sent', 'pending', 'sent_mid_turn', 'redeliver', 'skipped_late', 'skipped_paused',
                     'skipped_deleted', 'failed', 'answered', 'answered_mid_turn', 'unanswered'));
