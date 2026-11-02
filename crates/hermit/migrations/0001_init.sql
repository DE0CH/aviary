-- hermit's schema (hermit spec 10.2). A shipped migration is never edited: changes are new, additive files.

create table hosts (
  name text primary key,                                   -- 'hermit-gw', 'aws-<6 hex>'
  kind text not null check (kind in ('gateway','aws')),
  url text not null,                                       -- https://<wg ip>:7443
  wg_ip inet not null,
  wg_public_key text,
  instance_id text, private_ip inet,
  status text not null check (status in ('booting','active','retiring','gone')),
  retire_by text check (retire_by in ('operator','system')),
  max_sandboxes int check (max_sandboxes >= 0),            -- operator cap; null = by memory and CPU
  mem_total bigint, mem_reserve bigint, cpus int,
  info jsonb,                                              -- last GET /v1/host (13.4)
  created_at timestamptz not null default now(), last_seen_at timestamptz, empty_since timestamptz,
  gone_at timestamptz, gone_reason text
);
create unique index hosts_wg_ip_live on hosts (wg_ip) where status <> 'gone';

create table sandboxes (
  id uuid primary key,
  conversation_id uuid not null,
  person text not null,
  status text not null check (status in ('starting','ready','paused','releasing','gone')),
  phase text check (phase in ('capacity','workspace','placing','host','image','creating','supervisor')),
  version int not null default 1,
  name text not null unique,                               -- 'sbx-' || first 16 hex of the id
  token_kid smallint,                                      -- null only on imported history rows
  slot_claimed boolean not null default false,
  host text, ip inet,                                     -- no foreign key: imported history names old hosts
  image text, cpu real not null, memory_bytes bigint not null,
  busy boolean not null default false, busy_turn text, background_tasks int not null default 0,
  supervisor_version text, supervisor_protocol int, boot_id text,
  task text check (task in ('start','migrate','release')), -- the row claim (10.3)
  task_owner text, task_until timestamptz,
  migrating_to text, migration_started_at timestamptz,     -- set before a live migration's send (13.10)
  start_deadline timestamptz not null,
  reserved_until timestamptz,                              -- set by PUT (3.6.2)
  release_pending jsonb,                                   -- {reason, note, code, force, deadlineAt, askedAt, refusals}
  created_at timestamptz not null default now(), ready_at timestamptz,
  last_active_at timestamptz not null default now(), last_heartbeat_at timestamptz, last_busy_at timestamptz,
  expires_at timestamptz not null, hard_expires_at timestamptz not null,
  released_at timestamptz, release_reason text, release_note text, release_code text, release_message text,
  imported boolean not null default false
);
create unique index sandboxes_one_live on sandboxes (conversation_id) where status <> 'gone';
create unique index sandboxes_ip_live on sandboxes (ip) where status <> 'gone' and ip is not null;
create index sandboxes_live on sandboxes (status, created_at) where status <> 'gone';
create index sandboxes_claims on sandboxes (task_until) where task is not null;
create index sandboxes_person on sandboxes (person, created_at desc);
create index sandboxes_released on sandboxes (released_at) where status = 'gone';

create table sandbox_events (                              -- the feed (3.6.5)
  seq bigserial primary key, at timestamptz not null default now(),
  sandbox_id uuid not null, conversation_id uuid not null, person text not null,
  state text not null, phase text, version int not null, busy boolean not null,
  release jsonb
);
create index sandbox_events_at on sandbox_events (at);

create table sandbox_starts (                              -- today's ops_events kind sandbox_start (A3.F48)
  id bigserial primary key, at timestamptz not null default now(),
  ok boolean not null, ms int not null, start_ms int,
  sandbox_id uuid, conversation_id uuid, person text, host text,
  reason text, error text,                                 -- error redacted, <= 300 chars
  source_id bigint unique                                  -- the imported ops_events id (22.3)
);
create index sandbox_starts_at on sandbox_starts (at);

create table host_events (                                 -- today's vm_events (A3.F49)
  id bigserial primary key, at timestamptz not null default now(),
  kind text not null, host text, sandbox_id uuid, data jsonb not null default '{}',
  source_id bigint unique
);
create index host_events_at on host_events (at);

create table sandbox_logs (                                -- console tails (platform 15.4)
  sandbox_id uuid primary key, captured_at timestamptz not null default now(),
  bytes bytea, reason text
);

create table demand_hints (person text primary key, at timestamptz not null);       -- warm (3.5)

create table controls (                                    -- operator switches (16)
  id int primary key check (id = 1),
  sandboxes_enabled boolean not null default true, disabled_reason text,
  fresh_host_until timestamptz,
  updated_at timestamptz not null default now(), updated_by text
);
insert into controls (id) values (1);

create table admin_events (                                -- audit of every admin call (16), kept for good
  id bigserial primary key, at timestamptz not null default now(),
  actor text not null, action text not null, target text, data jsonb not null default '{}'
);
