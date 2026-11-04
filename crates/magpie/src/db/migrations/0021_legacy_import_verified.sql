-- legacy_import_verified: the last clean `magpie import verify --all` taken with every imported conversation owned by
-- magpie (cutover 7.7, 10.6; hermit 22.3 step 6): when, how many conversations, and the latest import change it
-- covered. `GET /v1/hermit/legacy-import` reads it for hermit's `legacy-cleanup`, which removes the old `users/` and
-- `sent/` trees only when it is complete. Unlike the window's other tables it outlives A6 (the cleanup runs at P6,
-- after `import_sources` is dropped); it is dropped with `legacy-cleanup` after P6.
create table legacy_import_verified (
  id            boolean primary key default true check (id),
  verified_at   timestamptz not null,
  conversations bigint not null,
  changes_until timestamptz            -- max(import_sources.last_change_at, first_imported_at) at the verify
);
