-- legacy_shares: the previous stack's shares as `hermit import-jarvis` last read them from its share table (hermit
-- 22.3 step 3): the share id, the workspace its frozen folder is cloned into (`uuidv5(SHARE_NS, 'share:' || id)`),
-- whether the share was revoked, and when the clone was made or found. Only committed, unrevoked shares are cloned.
-- `hermit legacy-cleanup` (22.3 step 6) reads it: every old share folder is either revoked or cloned before the old
-- trees go, and a clone magpie no longer names is removed. Dropped after the cleanup, with the subcommands.
create table legacy_shares (
  id text primary key,
  workspace uuid not null,
  revoked boolean not null,
  cloned_at timestamptz,
  seen_at timestamptz not null default now()
);
