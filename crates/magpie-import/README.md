# magpie-import

The one-off importer into magpie (cutover spec 6-7, magpie 19), linked into the `magpie` binary as
`magpie import …` and deleted after the switch (cutover 10.5 step 8). While it exists, the `magpie` binary itself is
built from this crate (`src/main.rs`; `.github/ci/build-magpie.sh` builds `-p magpie-import --bin magpie`), because
it links magpie's library and a package cannot depend on one that depends on it.

It reads the old stack read-only (database `cloudcli`: every `cc_u*` schema's `users`, `sessions` and
`jarvis_failed_turns`; the person's Claude Code files under `/jfs/users/<key>/claude/`) and writes through magpie's
own library, so an imported conversation is what live writes would have made: cc files through
`magpie::ccfs::ops` as `Caller::importer` (magpie 6.6/6.10's transaction without the lease check and with the
`migrating` check), events through `magpie::events::append`, changes through `magpie::changes`.

## Layout

| Path | What |
|---|---|
| `src/source.rs` | one `repeatable read`, read-only read of `cloudcli`; the timestamp rules (both text forms, the 1970 placeholder) |
| `src/tree.rs` | a conversation's source files and cc paths (main transcript, `tool-results/**`, `subagents/**`, the person's `memory/**`), stats, hashes, the workspace's `uploads/` |
| `src/sync.rs` | the sync of one file (cutover 7.4.2 rules 1-6), 16 MiB chunks, deterministic `opId`s |
| `src/conv.rs` | creating a migrating conversation, mapping a sessions row's changes, removing one whose row vanished (no hermit call) |
| `src/failed.rs` | failed turns as `user.sent` + `turn.ended` failed, sentences mapped back to codes, placed by time on the first import |
| `src/sent.rs` | the old hook's `sent/<conversation>/<toolUseId>/` snapshots as blobs, `sent_files` and `files.sent` (caption from the call's input) |
| `src/viewas.rs` | `jarvis.ops_events` view-as rows as `view_as_log` (the old `conversation` event is `send`) |
| `src/pass.rs` | one pass (`run`, `catch-up`), 8 conversations at a time |
| `src/verify.rs` | byte-exact verification through magpie's read path, the re-ingest, the released-source check |
| `src/plan.rs` | `plan`: counts, sizes, the largest lines, memory files, blockers |
| `src/control.rs` | `status`, `hold`, `reset --yes`, `unfence`, `release <id> --force` |
| `src/cli.rs` | `magpie import <command>`: one JSON document on stdout, logs on stderr |
| `tests/import.rs` | the end-to-end tests against the CI Postgres and S3 with fixtures made in the test |
| `tests/convergence.rs` | the property test of cutover 7.12 item 3 (random source changes, a pass at random points: byte-exact, visible events = the source's lines) |

## Where this differs from the spec

- `opId`s (cutover 7.4.2) also carry the file's version before the operation: without it, a later write of the same
  bytes at the same offset (a file rewritten back to what it held) would replay the first write's stored answer and
  write nothing. A repeat after a crash is still harmless (magpie's rule 0 takes an identical range as done).
- Rule 3 (append) compares the sha256 of the source's first `size(M)` bytes with the sha256 recorded at the last
  sync, rather than the last `k` bytes: a stronger check, same cost class.
- A sessions row that vanished while migrating removes the conversation's rows without any hermit call
  (`magpie::conversations::delete::forget_locked`): its workspace and its shares' clones are the old stack's until the
  release. `reset --yes` does the same.
- A `files.sent` event is appended at the next seq on the first import too (cutover 7.4.3 places it after the
  call); its `toolUseSeq` ties it to the call's tool row, which is where the timeline shows it.
- The view-as audit has no source id in magpie's log: a row is the same row when its time, admin, person, session
  and event are.
- Events of imported transcript lines carry the line's own `timestamp` as their time.
- Each conversation's sync ends by flushing the `truncated` limbo its writes left (one `retract`, as a turn's end
  does), so the visible events match the source after every pass rather than after magpie's 10 s sweeper.

## Not built yet (WP-29 continues)

Shares (7.4.4), `holds.js` and `holds`, the leader lock, `allow-handover`, `parity`, `reverse` and `export` (14.2-14.3), and
tests 4 (the routes of 6.4 beyond the write guard), 5-8 of 7.12. The hand-over loop itself is WP-52.
