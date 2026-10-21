// Postgres schema of the chat runtime (docs/CHAT.md, "Postgres schema"). Migrations run in order under an advisory
// lock, so two replicas starting at once apply each exactly once. Never edit a shipped migration: add the next one.
export const MIGRATIONS: string[] = [
  `
create table users (email text primary key, name text, is_admin boolean not null default false,
  created_at timestamptz not null default now(), last_seen_at timestamptz);
create table conversations (id uuid primary key, owner text not null references users(email),
  title text not null default '', model text not null, created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(), deleted_at timestamptz);
create index conversations_owner on conversations (owner, updated_at desc) where deleted_at is null;
create table messages (id bigserial primary key, conversation_id uuid not null references conversations(id),
  role text not null check (role in ('user','assistant')), content text not null,
  turn_id uuid, attachments jsonb not null default '[]', created_at timestamptz not null default now());
create index messages_conv on messages (conversation_id, id);
create table turns (id uuid primary key, conversation_id uuid not null references conversations(id),
  status text not null check (status in ('queued','running','waiting','done','error','cancelled')),
  token_hash text not null unique, replica text, model text not null,
  started_at timestamptz not null default now(), finished_at timestamptz,
  error text, usage jsonb, cost_usd numeric);
create index turns_conv on turns (conversation_id, started_at);
create unique index turns_one_active on turns (conversation_id) where status in ('queued','running','waiting');
create table events (id bigserial primary key, conversation_id uuid not null, turn_id uuid not null,
  seq int not null, type text not null, data jsonb not null, created_at timestamptz not null default now(),
  unique (turn_id, seq));
create index events_conv on events (conversation_id, id);
create table claw_sessions (conversation_id uuid primary key, content text not null,
  updated_at timestamptz not null default now());
create table files (conversation_id uuid not null, path text not null, sha256 text not null, size bigint not null,
  source text not null check (source in ('upload','agent')), updated_at timestamptz not null default now(),
  deleted boolean not null default false, primary key (conversation_id, path));
create table sandboxes (id uuid primary key, conversation_id uuid not null, pod text not null unique,
  egress_token_hash text not null unique, exec_token text not null,
  status text not null check (status in ('starting','ready','releasing','gone')),
  created_at timestamptz not null default now(), last_active_at timestamptz not null default now(),
  expires_at timestamptz not null, released_at timestamptz, release_reason text);
create unique index sandboxes_one_live on sandboxes (conversation_id) where status <> 'gone';
create table allowed_hosts (conversation_id uuid not null, host text not null, added_by text not null,
  created_at timestamptz not null default now(), primary key (conversation_id, host));
create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
  sandbox_id uuid, turn_id uuid, kind text not null, method text, host text not null, port int, path text,
  status int, decision text not null, bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric);
create index egress_log_at on egress_log (at);
create index egress_log_conv on egress_log (conversation_id, at);
create table tool_calls (id bigserial primary key, at timestamptz not null default now(), turn_id uuid,
  conversation_id uuid, user_email text, tool text not null, args jsonb, ok boolean, ms int, error text,
  cost_usd numeric);
create index tool_calls_at on tool_calls (at);
create table confirmations (id uuid primary key, conversation_id uuid not null, turn_id uuid not null,
  kind text not null, summary text not null, detail jsonb not null default '{}',
  status text not null check (status in ('pending','approved','denied','expired')),
  created_at timestamptz not null default now(), decided_at timestamptz, decided_by text);
create table kv (key text primary key, value jsonb not null);
`,
  // sandbox providers (docs/CHAT.md, "Sandbox providers"): how to reach a sandbox's agent, {url, headers}
  `alter table sandboxes add column agent jsonb;`,
  // sandbox provider "vm" (docs/SANDBOX-VM.md): the host a sandbox runs on now and its fixed overlay address; the hosts
  `
alter table sandboxes add column host text, add column ip text;
create unique index sandboxes_ip_live on sandboxes (ip) where status <> 'gone' and ip is not null;
create table vm_hosts (name text primary key, kind text not null check (kind in ('hub','aws')),
  url text not null, wg_ip text not null, wg_public_key text, instance_id text, private_ip text,
  status text not null check (status in ('booting','active','draining','retiring','gone')),
  max_sandboxes int, mem_total bigint, mem_reserve bigint, cpus int, info jsonb,
  created_at timestamptz not null default now(), last_seen_at timestamptz, empty_since timestamptz,
  gone_at timestamptz, gone_reason text);
create unique index vm_hosts_wg_ip_live on vm_hosts (wg_ip) where status <> 'gone';
`,
  // provider "vm": what the scaler decided and did (launches, drains, migrations with their pause, terminations)
  `
create table vm_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  host text, sandbox_id uuid, data jsonb not null default '{}');
create index vm_events_at on vm_events (at);
`,
  // the admin monitor (docs/CHAT.md, "Admin monitor"): sandbox starts (allocation latency) and failures with their reason
  `
create table ops_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  ok boolean, ms int, sandbox_id uuid, conversation_id uuid, host text, detail jsonb not null default '{}');
create index ops_events_kind_at on ops_events (kind, at);
`,
  // no confirmations and no per-conversation sandbox hosts any more (docs/CHAT.md: read-only tools, the automatic sandbox
  // allowlist): nothing reads or writes confirmations or allowed_hosts from here on. The tables stay for their old rows
  // (an older replica still running during the rollout reads them); nothing is left pending or granted.
  `
update confirmations set status = 'expired', decided_at = now() where status = 'pending';
update turns set status = 'running' where status = 'waiting';
delete from allowed_hosts;
comment on table confirmations is 'unused since schema 6: no confirmations (old rows only)';
comment on table allowed_hosts is 'unused since schema 6: the sandbox allowlist is automatic';
`,
  // the files a turn shows (docs/CHAT.md, "Presented files"): only what the agent passed to present_files, in its order,
  // [{path, caption?, sha256, size}]; null = a turn from before (the UI then shows that turn's changed files as it did)
  `alter table turns add column presented jsonb;`,
];
