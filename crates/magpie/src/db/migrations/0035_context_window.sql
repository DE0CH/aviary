-- The context window Claude Code runs the conversation's model at, as the supervisor's last turn end reported it
-- (its `result`'s `modelUsage`): what `context_tokens` is a share of in the app's context meter. Null until a turn
-- ends under a supervisor that reports it.
alter table conversations add column context_window integer;
