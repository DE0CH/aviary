-- A repeating schedule may start later (Deyao, 2026-11-17): its first fire is the rule's first time at or after this.
alter table schedules add column starts_at timestamptz;
