-- The slash commands the conversation's Claude Code last named (its `initialize` answer, in the supervisor's
-- `running` report): [{name, description, argumentHint}], for the app's `/` completion. Null before the first report.
alter table conversations add column commands jsonb;
