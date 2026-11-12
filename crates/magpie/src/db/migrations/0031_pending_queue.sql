-- The queue of messages sent while a turn ran is the server's (src/turns/midturn.rs): a message with attachments is
-- held too (its whole send request kept here) and starts a turn of its own when the running one has ended.
--   request  the M7 body of a message with attachments; null for a text message (its `text` says it all)
alter table pending_messages add column request jsonb;
