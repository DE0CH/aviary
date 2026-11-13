-- A conversation that changes harness carries over: the new harness reads what it has not seen as a file.
-- `admitted_harness` is the harness of the last turn admitted (null: none yet; `harness` then says what ran).
-- `carried` is, per harness, the event seq up to which it had seen the conversation when another harness took over
-- ({"claude": 412}): what it is handed when it is picked again starts after that.
alter table conversations add column admitted_harness text check (admitted_harness in ('claude', 'qwen'));
alter table conversations add column carried jsonb not null default '{}';
-- A share keeps which harness its conversation was in, with the other harnesses' sessions and what each had seen:
-- a conversation continued from it starts where the shared one stood.
alter table shares add column harness text not null default 'claude' check (harness in ('claude', 'qwen'));
alter table shares add column harness_sessions jsonb not null default '{}';
alter table shares add column carried jsonb not null default '{}';
