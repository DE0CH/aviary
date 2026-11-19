-- What hermit-jarvis last told magpie about the session's conversation (src/reconcile.rs): `open` (the session is
-- up: its person can message it), `closed` (paused or not running: the chat's message box says so and takes nothing)
-- or `ended` (the session is destroyed: the conversation left the chat).
alter table sessions add column chat text not null default 'open';
