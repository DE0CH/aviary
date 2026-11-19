-- A person pins a conversation to the top of their list (Deyao, 2026-11-19, design A): when they pinned it, null while
-- not pinned. The app shows the pinned ones in their own section, the most recent pin first.
alter table conversations add column pinned_at timestamptz;
