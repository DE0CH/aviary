-- The lines once classified as the echo of a person's prompt (magpie 6.5), by sha256, for the scope's life. A line
-- in limbo is matched there; once its limbo is flushed (the truncate's turn ended, or 10 s of quiet) only this table
-- says it was an echo, so a rewrite in a later person turn neither takes that turn's echo nor becomes a prompt event.
-- An echo line carries its uuid and timestamp, so two prompts with the same text are different lines.
create table cc_echoes (
  scope  uuid not null,
  sha256 bytea not null,
  primary key (scope, sha256)
);
