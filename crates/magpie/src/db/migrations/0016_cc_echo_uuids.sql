-- The uuids of the lines once classified as the echo of a person's prompt (magpie 6.5), for the scope's life, beside
-- their sha256 (`cc_echoes`): Claude Code can write the same message again with other bytes (another sessionId, a
-- field order, a resume's copy), and a line with an echo's uuid is that echo again, never a new prompt.
create table cc_echo_uuids (
  scope uuid not null,
  uuid  text not null,
  primary key (scope, uuid)
);
