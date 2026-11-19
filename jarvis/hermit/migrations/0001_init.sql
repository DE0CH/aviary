-- hermit-jarvis (aviary/README.md): a session is one conversation; a sandbox is one boot of the session's machine.

create table sessions (
  session_id text primary key,
  conversation_id uuid not null unique,
  person text not null,
  created_at timestamptz not null default now()
);

create table sandboxes (
  id uuid primary key,
  conversation_id uuid not null references sessions (conversation_id),
  session_id text not null references sessions (session_id),
  person text not null,
  -- the Fly machine that booted it; null while a resume is awaited
  machine text,
  state text not null,
  phase text,
  version bigint not null default 1,
  created_at timestamptz not null default now(),
  ready_at timestamptz,
  last_active_at timestamptz not null default now(),
  expires_at timestamptz not null,
  released_at timestamptz,
  busy boolean not null default false,
  image text,
  supervisor jsonb,
  -- {reason, note, code, message} once gone
  release jsonb,
  -- a release the supervisor has yet to finish: {reason, code, force, deadlineAt}
  pending jsonb,
  -- the descriptor's per-boot facts the machine sent (non-secret env, memory, cpus)
  boot jsonb not null default '{}'
);
create index sandboxes_conversation on sandboxes (conversation_id, created_at desc);
create unique index sandboxes_one_live on sandboxes (conversation_id) where state <> 'gone';

-- the change feed (hermit 3.6.5): one row per visible change
create table sandbox_events (
  seq bigserial primary key,
  at timestamptz not null default now(),
  body jsonb not null
);
