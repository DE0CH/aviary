-- Each replica's own view of what only it can see (hermit 15.6, 18.6): its workspace mount and its last refused
-- workspace delete, written every 10 s; the ops verdict reads the rows seen in the last 30 s, and a start whose
-- replica cannot reach the store hands over only while another replica reports a working one.
create table replicas (
  pod text primary key,
  seen_at timestamptz not null default now(),
  store_error text,                                        -- null = the mount works
  delete_refused_at timestamptz, delete_refused_path text
);
