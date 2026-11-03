-- The tables of the old stack's `jarvis` database that `hermit import-jarvis` reads, as the old chat schema made them
-- (jarvis3 chat/src/common/schema.ts, every migration applied), for the import's tests.
create table users (email text primary key, name text, is_admin boolean not null default false,
  created_at timestamptz not null default now(), last_seen_at timestamptz);
create table conversations (id uuid primary key, owner text not null references users(email),
  title text not null default '', model text not null, created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(), deleted_at timestamptz);
create table sandboxes (id uuid primary key, conversation_id uuid not null, pod text not null unique,
  egress_token_hash text not null unique, exec_token text not null,
  status text not null check (status in ('starting','ready','releasing','gone')),
  created_at timestamptz not null default now(), last_active_at timestamptz not null default now(),
  expires_at timestamptz not null, released_at timestamptz, release_reason text, agent jsonb, host text, ip text,
  owner text, user_key text);
create table vm_hosts (name text primary key, kind text not null check (kind in ('hub','aws')),
  url text not null, wg_ip text not null, wg_public_key text, instance_id text, private_ip text,
  status text not null check (status in ('booting','active','draining','retiring','gone')),
  max_sandboxes int, mem_total bigint, mem_reserve bigint, cpus int, info jsonb,
  created_at timestamptz not null default now(), last_seen_at timestamptz, empty_since timestamptz,
  gone_at timestamptz, gone_reason text);
create table vm_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  host text, sandbox_id uuid, data jsonb not null default '{}');
create table ops_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  ok boolean, ms int, sandbox_id uuid, conversation_id uuid, host text, detail jsonb not null default '{}');
