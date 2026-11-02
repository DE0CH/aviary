-- The admin reads (magpie 13, WP-15): what the last heartbeat said and when the lease was granted. Expand-only.
alter table conversations add column last_heartbeat   jsonb;        -- {gen, supervisor, cc} of the last heartbeat
alter table conversations add column lease_granted_at timestamptz;
