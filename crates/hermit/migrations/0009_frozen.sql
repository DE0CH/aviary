-- Freezing (jarvis4 docs/PLAN-fabric.md, "Next: freeze and the time machine"): the unused `paused` becomes `frozen`
-- (RAM, processes and scratch kept off any host), with when a freeze with `until` thaws by itself and where the
-- frozen sandbox is (`{object, key (sealed under hermit's key), frozenAt}`). A frozen row holds no host and no slot.
alter table sandboxes drop constraint sandboxes_status_check;
update sandboxes set status = 'frozen' where status = 'paused';
alter table sandboxes add constraint sandboxes_status_check
  check (status in ('starting','ready','frozen','releasing','gone'));
alter table sandboxes add column frozen_until timestamptz, add column frozen jsonb;
