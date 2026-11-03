-- A send's ticket call repeated with its `clientMessageId` gets the open ticket back only while that ticket is younger
-- than `magpie_api::legacy::TICKET_HOLD_MAX`, the age past which pigeon starts no write under it. An older one is
-- superseded instead (`superseded_at`): it stays open, since a write started under it may still be in flight, until
-- its outcome arrives or the importer expires it, and the repeat gets a new ticket (`crate::legacy::tickets::take`).
-- `legacy_tickets` holds no rows before the switch window opens (A2), so the index is built in the transaction.
alter table legacy_tickets add column superseded_at timestamptz;
drop index legacy_tickets_open_cmid;
create unique index legacy_tickets_open_send_cmid on legacy_tickets (conversation_id, client_message_id)
  where done_at is null and client_message_id is not null and superseded_at is null;
