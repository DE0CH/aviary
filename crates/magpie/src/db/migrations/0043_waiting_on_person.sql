-- A command in the conversation's sandbox waits on the person (Deyao, 2026-11-18): the supervisor's heartbeat says
-- so; the app's list shows the conversation as needing the person. Cleared with the sandbox.
alter table conversations add column waiting_on_person boolean not null default false;
