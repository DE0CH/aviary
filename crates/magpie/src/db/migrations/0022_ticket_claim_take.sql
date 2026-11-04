-- The take a send's write was last claimed under (`claim_take`, set with `claimed_at`): a `refused` from the latest
-- take closes the ticket only when no earlier take claimed it, since that earlier holder may have written and its
-- write may be running (`crate::legacy::tickets::done`). An expand step: one nullable column, dropped with
-- `legacy_tickets` in A6.
alter table legacy_tickets add column claim_take uuid;
