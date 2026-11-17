-- sandbox_starts.person_limit: the start was refused because its person already held their own number of sandboxes
-- (`sandbox.maxActiveByPerson`), not because the pool was full or the start broke. The operator snapshot leaves such a
-- refusal out of the failed starts for a person whose `sandbox.notifyPersonLimit` is false (it is still listed, quiet).
alter table sandbox_starts add column person_limit boolean not null default false;
