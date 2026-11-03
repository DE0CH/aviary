-- A ticket can be handed to more than one request (a send's repeat within the reuse age), so each hand-out gets a
-- take of its own (`take`, the latest) and a send's write is claimed with it right before it starts (`claimed_at`):
-- only the latest take may claim, a `refused` from an older take changes nothing, and a reuse waits until a claimed
-- write has settled (`crate::legacy::tickets`). An expand step: two nullable columns, dropped with `legacy_tickets`
-- in A6.
alter table legacy_tickets add column take uuid, add column claimed_at timestamptz;
