-- The supervisor process that acknowledged a command (AckRequest.by: its boot and keeper restarts), null from an
-- older supervisor. A heartbeat built before a start_turn's ack can land after it; only a process started after the
-- one that acknowledged can have lost the turn (magpie 8.5 rule 2; 2026-11-27: a chaos run's turn failed this way).
alter table commands add column acked_by_boot text, add column acked_by_restarts integer;
