-- A second harness beside Claude Code (Qwen Code). The model a person picks names the harness the conversation's
-- next turn runs in (`picked_harness`, with `picked_model`). `harness` is the one that ran last: the one
-- `provider_session_id` and `main_transcript` belong to. `harness_sessions` keeps the session id of each harness
-- that ran before and is not the current one ({"qwen": "<session id>"}), for a pick back.
alter table conversations add column picked_harness text not null default 'claude'
  check (picked_harness in ('claude', 'qwen'));
alter table conversations add column harness text not null default 'claude' check (harness in ('claude', 'qwen'));
alter table conversations add column harness_sessions jsonb not null default '{}';
