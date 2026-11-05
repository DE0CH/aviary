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
| `src/verify.rs` | byte-exact verification through magpie's read path, the re-ingest, the released-source check, 7.7's sanity notes (event and prompt counts against the classifier's count of the source, a workspace not on disk), and the record a clean `verify --all` leaves when the import is complete (`legacy_import_verified`, read by hermit's `legacy-cleanup` through `GET /v1/hermit/legacy-import`) |
| `src/plan.rs` | `plan`: counts, sizes, the largest lines, memory files, blockers |
| `src/holds.js`, `src/holds.rs` | the running sets (cutover 6.3): the script run in every old ui pod (each CloudCLI child asked for its running list, `ok: false` when it does not answer in 5 s; an answered parked question of `asking` counts as running), its output, the fail-closed merge |
| `src/questions.rs` | the old stack's parked questions (`jarvis_open_questions`, DECISIONS 17): read inside the release's fence (an answered one keeps an idle release from happening), carried over in the release transaction as a parked turn with one open ask per question, keyed by its `tool_use_id` (magpie 8.1a) |
| `src/kube.rs` | the old cluster's API with `reqwest` and `tungstenite` (the Kubernetes crates are hermit's): ui pods in every phase, `pods/exec` over `v4.channel.k8s.io`, `pods/proxy` to the old router's switch module (`x-jarvis-switch-token`: the report and `restart`); bindings `MAGPIE_IMPORT_LEGACY_KUBE_URL`, `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN`, the CA at `/etc/legacy-kube-ca/ca.crt` |
| `src/lock.rs` | the leader lock (`hashtext('magpie-import.leader')`): the hand-over replicas' bid, a one-shot command's wait (seen in `pg_locks`), the waiter check the loop yields to |
| `src/control.rs` | `status`, `hold`, `reset --yes`, `unfence`, `release <id> --force`, `purge <id>…` |
| `src/handover.rs`, `src/gate.rs`, `src/server.rs` | the hand-over loop (7.5): its pass, DESIGN 9.4's release, the per-person gate (`allow-handover --person`, table `import_gate_persons`), the recovery of fences a killed lock holder left, `allow-handover`; the leader loop (letting the lock go to a waiting one-shot command between passes), `:9090/status`, the writer sidecar's `export-server` |
| `src/reverse.rs` | `reverse <id>…` (14.2): the old child's running set, magpie's reverse fence (renewed every second), the export by the writer sidecar (pod-local `POST /v1/export`), the old router's `restart` on every ui pod, the reopen into legacy with `import_sources` synced to what the export wrote |
| `src/export.rs` | the reverse export (14.3): a reverse-fenced owned conversation's cc files (memory to `aviary-memory/<id>/`; old-side files its scope no longer has deleted), its sessions row (with `project_path`), its failed turns and sent files written back to the old side (temporary file, then rename; 0600, uid 1000, or an error) |
| `src/parity.rs` | `parity <id>…` (10.2 rehearsal 6): magpie's rows through pigeon's own `PaseoProjector` (zh), each normalised to `[kind, role, text sha256, tool name, tool status, attachment names]` |
| `src/cli.rs` | `magpie import <command>`: one JSON document on stdout, logs on stderr; every command but `plan` and `status` holds the leader lock |
| `tests/import.rs` | the end-to-end tests against the CI Postgres and S3 with fixtures made in the test |
| `tests/shares.rs` | prefix and non-prefix frozen transcripts, revoked before and after the import, a share of a gone conversation, `reset` |
| `tests/export.rs` | 7.12 item 7: import, release, grow the conversation on the new side, export (refused unfenced), the old side's files equal magpie's reconstruction, a stale old-side file deleted, the sessions row and the failed turn match, memory in `aviary-memory/<id>/`, a re-import verifies; a purge on the new side is not re-imported by a pass before the export |
| `tests/handover.rs` | `release --force`'s final sync inside the fence, `verified_stats`' fingerprints, the re-ingest's record and limbo, a rename onto an existing path, a released conversation's check (shared memory left out, new files seen), the mount check, the sent folders' quiet period, the lost lock, read-only sources, a cleared title, non-UTF-8 names |
| `tests/loop.rs` | 7.12 item 5: the hand-over loop against a fake Kubernetes API (ui pods, one Terminating; exec answering `holds.js`; `pods/proxy` answering `/__jarvis/sockets`) and a fake `jarvis.sandboxes`: nothing before the gate; `allow-handover` refused while a pod has no report; a socket or HTTP request not LU, an old sandbox not `gone`, an unanswered child each hold the release; two clean reads release the idle conversation and never its running sibling; a parked old question is carried over (its answer on the new side resumes the turn with a `start_turn` carrying it) while an answered one keeps its conversation legacy; a source change waits for a pass that finds it unchanged; a per-person gate releases that person's conversations only, judged on their part of every report each pass; a leader killed between fence and release (its pass dropped inside the fence) is finished by the next one (re-stat and release, or a change that lifts the fence, or a lapsed fence cleared) |
| `tests/reverse.rs` | 14.2: a reverse that waits while the person's old child runs something or does not answer, and while `restart` answers `busy` (exported, the fence lifted, native writes go on); a native write during the reverse fence waits past one lease and then answers 409 `route: "legacy"`; the reopened conversation imports nothing again on the next pass and verifies; a conversation born on the new side gets its `import_sources` row; a person with no old schema is refused |
| `../magpie/tests/fence_race.rs` | 7.12 item 6 on magpie's side: the fence-race fuzz (1 000 sends around fences that release or lift, a third with a magpie-only file copied under the send's ticket), a database switchover during a fence (within and past the lease), the reverse fence's waits, the per-person gate in the window state |
| `tests/holds_js.rs`, `tests/kube.rs` | `holds.js` against fake CloudCLI children (node), the transport against a fake Kubernetes API |
| `tests/convergence.rs` | the property test of cutover 7.12 item 3 (random source changes of several line kinds, lines written again as another kind, a pass at random points: byte-exact, visible events (kind, uuid) = magpie's classifier over the source's lines) |

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
  live one (all five live shares on 2026-11-03) that is exactly the frozen file. One that is not is a `plan` blocker
  and the share's error in every pass (never cut to the common lines, which would drop what the old page shows
  after them); a hidden copy of the frozen files would have to be a conversation that is neither listed nor in
  Recently deleted, which magpie's model has no state for (a deleted source answers 404, magpie 12.3). The
  snapshot's `tool-results/` and `subagents/` are the conversation's at import time (a superset of the frozen
  `session/`).
- A share whose conversation's sessions row is gone (missing from a read with no problem for its owner's schema) is
  stored revoked (`deleted`), as the old stack answers 404 for it; one whose conversation is only missing for now
  waits, and one stored `deleted` is imported live when its row appears. `reset --yes` removes the imported shares
  with their conversations.
- `verified_stats` is `{files: {<cc path>: [size, mtime_ns, ino]}, rowFp, failedTurns, sentFp}` from verification's
  own read; verification passes only when the row, failed turns and sent folders are what the last pass synced.
  `verify::changed_since_verify` and `verify::changed_since_sync` are the release's re-stat (7.8).
- `release <id> --force` verifies (reported in its JSON, not a refusal) and syncs once before the fence (so the
  sync inside it usually has nothing to do); a release that cannot happen (the mount, the read, the sessions row
  gone) fails there. Then it expires the open tickets, so an old turn still running is cut off: forcing is the
  operator's call (Deyao, 2026-11-03: forcing may disrupt what is running). It fences, syncs once more inside the
  fence and re-stats; a change inside the fence lifts it and fails. The conversation's own sync error (a late sent
  file, a main transcript gone, a name that is not UTF-8, a write magpie refused) is reported in `syncError` and
  released past, as a verification mismatch is: forcing is the way out for a conversation stuck on one, and what
  that sync could not bring over stays on the old side only. The re-stat is then against the sources as they were
  when the fence was taken (none when they cannot be stated at all, said in `syncError`). The release is one transaction
  that also checks the lock; a final sync that outlasted the fence's 3 s lease fails the release with that said
  (nothing released; run it again). Its JSON has `fenceSeconds` (the database's fence-to-release time) and
  `inFenceSeconds` (the importer's own). The `magpie_fence_seconds` observation of a one-shot command goes to that
  process's registry, which nothing scrapes: forced releases are counted in the JSON only and are kept out of the A5
  gate's histogram (the import Deployment's `:9090`) on purpose. 7.8's upload-ingestion pass inside the release
  transaction is not run here (WP-52's); until it exists a native send naming an old asset of a conversation
  released by force waits for the import Deployment's synchronous ingest (A3 on).
- After a forced release, what the old side writes to the conversation (the cut-off turn going on) is reported in
  `forcedWrites`, not in `mismatches`: `verify --all` passes on it and the hand-over is not held; those writes are
  lost, as forcing accepted. After a hand-over's (`idle`) release the same writes are mismatches.
- A `sent/<conversation>/<toolUseId>/` folder is imported once nothing in it (its files and every folder in it)
  changed for 15 s (`sent::QUIET`); until then the recorded `sent_fp` carries `waiting:` so the next pass looks again
  and verification waits. A folder with no files is not imported (no `files.sent` without files) and one whose call
  has an error result in the transcript (the hook refused the send but left its snapshot) never is. "Imported" is
  per (call, path): a file that appears in a folder after its import is the conversation's error and its `sent_fp`
  carries `late:`, so verification never passes on it (an operator decides: `release <id> --force` releases past it).
- A sessions row id in two schemas (or twice in one) is a `plan` blocker and neither row is synced; a write for one
  person to another person's conversation is the conversation's error, never taken for a release.
- A pass purges an open conversation missing from the read only when its own schema was read: a schema gone or
  renamed gives each of its conversations an error. A read with no person schemas while conversations are open, or
  one missing more than `pass::PURGE_MAX` (3) conversations, or two or more that are over half the open ones,
  purges nothing (an error `purge`, and one per missing conversation). `purge <id>…` is the operator's purge of
  such a conversation: only when a fresh, clean read of its own schema has no row for it.
- Auto-memory of a conversation the reverse export wrote back: once `aviary-memory/<id>/` is there, that
  conversation's memory (cc paths `projects/-workspace/memory/**`) is that folder's files and, over them, every file
  of the person's shared folder written after the export (the folder's newest write): a conversation reopened after
  a rollback keeps the memory it had on the new side (files made there included) and gets what its own old turns
  write, which can only go to the shared folder. Nothing tells those writes from other conversations' old turns
  writing the shared folder after the export, so theirs come too; a file deleted from the shared folder stays in
  the conversation. The spec says the importer ignores `aviary-memory/` (14.3): it still does for every other
  conversation.
- Every pass, `final sync` and export first checks that `/jfs/users` holds something (an empty mount point would read
  as "everything deleted"), and a main transcript that vanished while its sessions row is there is the conversation's
  error, not an unlink. A name that is not UTF-8 is an error, never a skipped file.
- One-shot commands hold the leader lock on one connection and check it (`pg_locks`, this backend) before each
  conversation's write, inside each of the importer's own write transactions (create, row, record, purge, failed
  turns, sent files, the limbo flush, verification's record, the export's release record, the forced release), and
  right before each of magpie's cc writes (`ops::write`, `truncate`, `unlink`, `rename`, `mkdir`, whose transactions
  are magpie's own, so a lock lost inside one of those is caught at the next check). A lost lock stops the command
  (`Error::LockLost`). Verification records its outcome only when no pass synced the conversation since its read
  (`last_synced_at` unchanged; else the conversation's error, verify it again). The old databases are opened with
  `default_transaction_read_only = on` (`aviary_pg::connect_read_only`) for every command but `export`.

- `export` does not fence: the reverse hand-over (14.2, WP-52) fences (`legacy_fence_at` on the owned conversation),
  calls it and unfences. A target without a live fence (set, younger than its 3 s lease) is the export's error and
  is not written, unless `--unfenced`. The export renews the lease before each file, before the `cloudcli` writes
  and at the end (a lapse is the conversation's error), and compares the conversation's head seq, cc file versions,
  row (title, `name_source`, Recently deleted, session id, model, activity) and shares before and after its writes,
  reading what it writes only after the first snapshot: a change is the conversation's error, to be exported again
  (magpie's native writes wait in their call while the reverse fence is live, so that check catches only a write
  through a lapsed fence). Each exported conversation's result has `recorded`, the
  files written with their stat and sha256 in `import_sources.files`' shape, for the reopen (14.2 step 5). For an
  imported (released) conversation the export also makes that its release record (`import_sources.files`, the
  deleted paths taken out; also when it stops part-way), so a conversation exported and left owned (14.2 step 4's
  `busy`, a failed export) verifies against what the export wrote, and what the old stack writes after it is still
  a mismatch. With `--since <t>` the targets are the owned conversations created after `t` or with an event, a
  change (magpie's `changes` feed: a move to Recently deleted and a restore append none) or a share made or revoked
  after it. The export's temporary files (`.<name>.aviary-export-<hex>`) are removed on every error, never read as
  sources (`tree::is_export_temp`), and swept by the next export of the conversation once older than
  `export::TEMP_AGE` (a crash's).
  The CLI's `export` takes the leader lock like every writing command; WP-52's `export-server`, run inside the
  hand-over leader that holds it, calls `export::export` directly with `lock: None`, or it would wait for itself.
- One Claude Code session id, one old-side transcript: a person who continued a share twice (or their own) has
  conversations with the same `provider_session_id` (magpie does not rewrite it on continue, magpie 5 item 5),
  which the old side would read from one `projects/-workspace/<psid>.jsonl`. Only the person's earliest
  conversation with that id is exported under it; each other one is the export's error. A permanent deletion keeps
  the transcript and session folder of a session id another conversation of the person still has. Open (a design
  gap, raised with Deyao): such a later conversation has no old-side copy, so a rollback leaves its new-side history
  out of the old stack until the earlier one is purged; exporting it under its own session id would rewrite every
  line's `sessionId` and its cc paths, which the import's byte-exact record of it would then not match.
  Without `--since` it exports every owned conversation. A failed turn is exported when it never reached Claude Code
  (no Claude Code event in the turn: those are the ones the old stack kept in `jarvis_failed_turns`), with the
  Traditional Chinese sentence of its code, once per `error_at` (a turn with no end time is not exported). Old-side
  files the conversation's scope no longer has (under its session folder, its `aviary-memory/<id>/`, or recorded by
  the importer) are deleted: only under the session folder's `tool-results/` and `subagents/` (what the importer
  imports of it); any other file there is never in magpie's scope and stays. The sessions row gets `project_path = '/workspace'`, as every old row has, and the
  schema a `projects` row for it when it has none.
- Shares made on the new side go back in the old layout (`users/<key>/shares/<id>/`: `transcript.jsonl` and
  `session/` from the share's snapshot scope, `sent/` from the blobs of the sends within it, `ws/` cloned from the
  share's workspace with `juicefs clone`) and a `cc_shared.shares` row with the same id. No `messages.json` is
  written: magpie does not hold `CloudCLI`'s message format, and the old share page builds a snapshot's history
  from its transcript and session folder when that file is absent (DE0CH/claudecodeui 71b65cf `frozenHistory`).
  Until the old ui runs that commit such an exported share answers 404 on the old stack: the A1 ui rollout's pin
  must include it, and the runner's preflight checks that before any reverse export (WP-56/WP-40's, not this
  crate's). A share revoked on the new side is revoked on the old side (row marked, folder removed).
- Deletions for good on the new side: magpie's purge (not the importer's forget) writes a tombstone to
  `import_purged` (conversation, person, Claude Code session id, cc paths) while that table exists (dropped in
  A6). `export` deletes the old sessions row, its failed turns, its transcript and session folder, its
  `aviary-memory/<id>/` and `sent/<id>/`, and revokes its old share links; the person's shared memory folder is
  never touched, and `ws/<id>` is hermit's (the purge's own outbox call). A tombstone whose id is a conversation in
  magpie again is skipped.
- The import image carries the `JuiceFS` client at `/usr/local/bin/juicefs` (`cli::JUICEFS`), which the export's
  share clone runs through the mount's `.control` file (no credentials).
- A sent-file snapshot's time is its folder's mtime to the nanosecond.

- `reverse` (14.2) makes one attempt per conversation and answers `waiting` (still owned and native, nothing
  changed) when the person's old child runs something, a child did not answer, the conversation is busy on the new
  side, or `restart` answers `busy`; waiting and retrying is the caller's (`cutover rollback conversation`). It runs
  in the hand-over container (the old API's bindings and `MAGPIE_IMPORT_TOKEN_UI`) holding the leader lock, and the
  export runs in the writer sidecar through the pod-local `127.0.0.1:8081` (`MAGPIE_IMPORT_EXPORT_URL` overrides),
  which needs no token: only the pod's own containers reach it. `restart` is called on every ui pod listed (one with
  no child of the person answers `none`), so a child started after the running-set read is stopped too, or holds the
  reverse. A person with no schema on the old side is refused: the export writes no sessions row for them, and the
  old stack's disk discovery would index the transcript under an id that is not the conversation's. The reopen
  stores, beside the export's files, the sessions row's, failed turns' and sent folders' fingerprints as the next
  pass reads them, so the exported failed turns and sent files are not imported back as new ones. Reverse-fenced,
  an owned conversation also refuses a sandbox lease, and a native write waits up to `limits::REVERSE_FENCE_WAIT`
  (25 s) while the lease is renewed, then answers 409 `conversation_migrating` with `retryAfterMs`.
- The hand-over leader lets the lock go between two passes when a one-shot command waits for it (`lock::waiter_present`)
  and bids again at once; the waiter is granted the lock as it is let go, so the loop resumes once the command ends.
- A pass starts by settling the fences a previous lock holder left (`handover::recover`): a live hand-over fence is
  re-stated and released (or lifted on a change, an error or `hold on`), a lapsed one cleared, a reverse fence lifted.

## Owned by other packages

The hand-over loop, `allow-handover` (with `--person`), `uploads`, `reverse` and 7.12 item 6's fence cases are
WP-52's, all built. WP-54 owns the move fuzz.
