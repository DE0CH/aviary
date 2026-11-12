-- A conversation the deployment has closed to its person's messages (hermit's route; a session paused elsewhere):
-- the sentence the app shows in place of its message box. Null: open.
alter table conversations add column closed_reason text;
