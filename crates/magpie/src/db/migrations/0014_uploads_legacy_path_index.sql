-- aviary-pg: no-transaction
-- The legacy-path lookup (`crate::legacy::paths::lookup`, DESIGN 9.5): the newest teed upload of a person under an old
-- asset path. Built concurrently: `uploads` is populated, and a plain build would hold off every upload while it runs
-- (platform expand rules).
create index concurrently if not exists uploads_legacy_path on uploads (person, legacy_path, created_at desc)
  where legacy_path is not null
