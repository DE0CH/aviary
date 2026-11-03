# magpie-import

The one-off importer into magpie (cutover spec 6-7, magpie 19), linked into the `magpie` binary as
`magpie import …` and deleted after the switch (cutover 10.5 step 8). While it exists, the `magpie` binary itself is
built from this crate (`src/main.rs`; `.github/ci/build-magpie.sh` builds `-p magpie-import --bin magpie`), because
it links magpie's library and a package cannot depend on one that depends on it. It also links pigeon's library,
for `parity` only (the projection pigeon serves).

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
| `src/share.rs` | `cc_shared.shares` as magpie shares with the same ids (7.4.4): frozen from magpie's copy of the conversation up to where the share's `transcript.jsonl` ends, revoked ones revoked, ones of a conversation that is gone stored revoked |
| `src/viewas.rs` | `jarvis.ops_events` view-as rows as `view_as_log` (the old `conversation` event is `send`) |
| `src/pass.rs` | one pass (`run`, `catch-up`), 8 conversations at a time |
| `src/verify.rs` | byte-exact verification through magpie's read path, the re-ingest, the released-source check, and 7.7's sanity notes (event and prompt counts against the classifier's count of the source, a workspace not on disk) |
| `src/plan.rs` | `plan`: counts, sizes, the largest lines, memory files, blockers |
| `src/holds.js`, `src/holds.rs` | the running sets (cutover 6.3): the script run in every old ui pod (each CloudCLI child asked for its running list, `ok: false` when it does not answer in 5 s), its output, the fail-closed merge |
| `src/kube.rs` | the old cluster's API with `reqwest` and `tungstenite` (the Kubernetes crates are hermit's): ui pods in every phase, `pods/exec` over `v4.channel.k8s.io`; bindings `MAGPIE_IMPORT_LEGACY_KUBE_URL`, `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN`, the CA at `/etc/legacy-kube-ca/ca.crt` |
| `src/lock.rs` | the leader lock (`hashtext('magpie-import.leader')`): the hand-over replicas' bid, a one-shot command's wait (seen in `pg_locks`), the waiter check the loop yields to |
| `src/control.rs` | `status`, `hold`, `reset --yes`, `unfence`, `release <id> --force` |
| `src/export.rs` | the reverse export (14.3): an owned conversation's cc files (memory to `aviary-memory/<id>/`), its sessions row, its failed turns and sent files written back to the old side (temporary file, then rename; 0600, uid 1000 when root) |
| `src/parity.rs` | `parity <id>…` (10.2 rehearsal 6): magpie's rows through pigeon's own `PaseoProjector` (zh), each normalised to `[kind, role, text sha256, tool name, tool status, attachment names]` |
| `src/cli.rs` | `magpie import <command>`: one JSON document on stdout, logs on stderr; every command but `plan` and `status` holds the leader lock |
| `tests/import.rs` | the end-to-end tests against the CI Postgres and S3 with fixtures made in the test |
| `tests/shares.rs` | prefix and non-prefix frozen transcripts, revoked before and after the import, a share of a gone conversation, `reset` |
| `tests/export.rs` | 7.12 item 7: import, release, grow the conversation on the new side, export, the old side's files equal magpie's reconstruction, the sessions row and the failed turn match, memory in `aviary-memory/<id>/`, a re-import verifies |
| `tests/holds_js.rs`, `tests/kube.rs` | `holds.js` against fake CloudCLI children (node), the transport against a fake Kubernetes API |
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

- A share is frozen from magpie's copy of its conversation rather than from a hidden copy of its frozen files
  (magpie 19.1): its `upto_seq` covers the events of the main-transcript lines the frozen `transcript.jsonl` holds
  (matched line by line by sha256 from offset 0, up to the next line that has an event), and its snapshot scope is
  the conversation's scope with the main transcript cut there. For a frozen transcript that is a byte prefix of the
  live one (all five live shares on 2026-11-03) that is exactly the frozen file. For one that is not, the share keeps
  the common lines (`shares.notPrefix` in the pass report), because a hidden copy would have to be a conversation
  that is neither listed nor in Recently deleted, which magpie's model has no state for (a deleted source answers
  404, magpie 12.3). The snapshot's `tool-results/` and `subagents/` are the conversation's at import time (a
  superset of the frozen `session/`).
- A share whose conversation has no sessions row is stored revoked (`deleted`), as the old stack answers 404 for it.
  `reset --yes` removes the imported shares with their conversations.

- `export` does not fence: the reverse hand-over (14.2, WP-52) fences, calls it and unfences. Without `--since` it
  exports every owned conversation. A failed turn is exported when it never reached Claude Code (no Claude Code
  event in the turn: those are the ones the old stack kept in `jarvis_failed_turns`), with the Traditional Chinese
  sentence of its code, once per `error_at`.
- Shares made on the new side go back in the old layout (`users/<key>/shares/<id>/`: `transcript.jsonl` and
  `session/` from the share's snapshot scope, `sent/` from the blobs of the sends within it, `ws/` cloned from the
  share's workspace with `juicefs clone`) and a `cc_shared.shares` row with the same id. No `messages.json` is
  written: magpie does not hold `CloudCLI`'s message format, and the old share page builds a snapshot's history
  from its transcript and session folder when that file is absent (DE0CH/claudecodeui `frozenHistory`, shipped by
  the A1 ui rollout). A share revoked on the new side is revoked on the old side (row marked, folder removed).
- Deletions for good on the new side: magpie's purge (not the importer's forget) writes a tombstone to
  `import_purged` (conversation, person, Claude Code session id, cc paths) while that table exists (dropped in
  A6). `export` deletes the old sessions row, its failed turns, its transcript and session folder, its
  `aviary-memory/<id>/` and `sent/<id>/`, and revokes its old share links; the person's shared memory folder is
  never touched, and `ws/<id>` is hermit's (the purge's own outbox call). A tombstone whose id is a conversation in
  magpie again is skipped.
- The import image carries the `JuiceFS` client at `/usr/local/bin/juicefs` (`cli::JUICEFS`), which the export's
  share clone runs through the mount's `.control` file (no credentials).
- A sent-file snapshot's time is its folder's mtime to the nanosecond.

## Owned by other packages

`allow-handover` and `reverse` (14.2, with WP-52's loop), and tests 4 (the routes of 6.4 beyond the write
guard), 5-6 of 7.12 and item 8's process-level half (the loop's yield between passes). The hand-over loop itself is
WP-52.
