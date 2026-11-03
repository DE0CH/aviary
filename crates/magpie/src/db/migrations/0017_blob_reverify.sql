-- Blobs the GC must re-verify against the bucket (magpie 5.3): it lost a blob's lock while deleting its objects, so a
-- delete may have landed after a writer's PUT. A row stays until the check has run under the blob's lock (blobs::gc
-- reverify); every GC pass retries what is left.
create table blob_reverify (
  sha256 bytea primary key,
  since  timestamptz not null default now()
);
