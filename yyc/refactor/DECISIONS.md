# Deyao's decisions on the design's open questions (2026-11-02)

1. Deploys: GitHub workflows (the 2026-10-28 rule). A push to DE0CH/YYC applies yyc-ai through a workflow; the Flux
   Kustomization yyc-ai is removed.
2. magpie's Postgres: its own 3-instance synchronous cluster magpie-pg (EUR 2.06/month).
3. DE0CH/aviary licence: AGPL-3.0-or-later.
4. Switch-over: NO gaps allowed. Nothing read-only, nothing cut, no "being moved", no redone attachments or view-as.
   G1-G5 of DESIGN.md 9.4 must be designed away.
5. Old chat UI data (database jarvis, bucket jarvis/): archive dump only, nothing imported, then deleted.
6. Claude Code auto-memory: per conversation.
7. Leavers' conversations: manual operator purge only.
8. Shared cluster pg: made highly available (EUR 13.73/month plus the node memory it needs).
9. Timing: because the switch has no gaps, it runs whenever it is ready; no 04:00 HKT window or quiet-hours
   requirement (Deyao, 2026-11-02). Working hours were allowed, never required: there is no time window at all
   (Deyao, 2026-11-06).
10. (Revises 8, Deyao 2026-11-02) The shared cluster pg stays single-instance: the HA plan costs EUR 98.90/month
    (two cpx32 nodes). magpie-pg stays its own 3-instance cluster (EUR 2.06/month).
11. Brief reconnects are acceptable during the switch (revises the strictness of 4): changes reach the old ui router
    through one ordinary ui rollout (every open tab reconnects once); open old tabs are moved right away with a ~2 s
    "reconnecting" notice (no 72 h wait); a rollback may refresh the UI once. Still not allowed: read-only
    conversations, cut or lost running turns, lost input, failed attachments, lost asks.
12. Rehearsal R5 (hand over one admin a day early): skipped.
13. (Deyao, 2026-11-03) YYC's cluster moves to a new Talos cluster with three control nodes (plan by session
    s4ba5e76eb6ec7ca4, "Kernel patches on K8s runners": https://1e6d165c-page.deyaochen.com/; that session builds it).
    The refactor's new stack (pigeon, magpie, magpie-pg, hermit, the new egress/tools, the sandbox gateway as a
    host-network pod) deploys straight onto the NEW cluster (plan step 3); the old stack stays on the old cluster, so the
    switch also moves YYC AI between clusters and YYC AI moves only once. The operator phase (WP-39/40 onward) starts
    only after the new cluster's steps 1-2 (control plane built and tested, first workers) are done; read that session's
    transcript for its state. Nodes may span locations (CLAUDE.md, 2026-11-03).

## Claude's engineering calls (Deyao: asking first is for providers, credentials, accounts and spend)

E1. (2026-11-03) DESIGN 14 question 2, the tombstone: accepted without triggering it on the mount. The mount supports
    the rewrite (performRemoveByUuid's open r+/read/ftruncate/pwrite replayed byte for byte through it, and Claude
    Code resumed the result; GATE.md items 2, 9), and 2.1.304's only firing trigger (the refusal fallback) retracts an
    unwritten message (rtomb-t5/t6: no rewrite, no truncate). Whichever path a version takes, the mount serves it.
    Settles the gate's deviation 5. A later Claude Code version is re-checked by WP-24's image smoke.
E2. (2026-11-03) WP-22 review 3 deviations accepted: (4) a supervisor restarted during a magpie outage serves nothing
    until magpie answers (consistent with decision "a write waits"); (5) a kept interrupted result is not dropped by
    the thread's non-mutating requests.
E3. (2026-11-03) DESIGN 12 D25: WP-17's frame validation with the clients' own zod validators replaces spec 18.1's
    serde + jsonschema-crate test (it checks against what the shipped apps accept).
E4. (2026-11-04) WP-50 review: pigeon reopens an LU that drops under a live tab on its own (no reconnect) when the
    holder's newest token lives at least 1 h more and the LU had been up 5 s; otherwise the tab is closed 1012 and
    reconnects through the edge. This departs from DESIGN 9.3/9.5 ("LU is reopened only behind a reconnect that came
    through the edge") and WP-50's "never reopened for a person whose token expires within the window": with the LU
    crossing bridge B1 between two clusters (decision 13), a blip would otherwise reconnect every tab on it, which 9.3
    allows only for the move. The old router still checks the token on each upgrade, and a refused or missing token
    gives the signed-out answer (C17). Recorded in DESIGN 9.5; WP-54 and R6 test against it.

E5. (2026-11-04) The A1 ui gate (DESIGN 9.6) waits only on running work in the pod it is about to restart: a running
    turn, a held Claude Code process, a sandbox preparing, a pending message (the switch busy route), never on a
    connected tab. Decision 11 lets every open tab reconnect once in the one ordinary ui rollout; what it forbids is a
    cut turn, lost input or a lost ask. Waiting on tabs would hold the gate for as long as any tab stays open (WP-54's
    finding: a tab on old pod B keeps B busy), so "reconnects at most twice" could never be met.
14. (Deyao, 2026-11-03) The relay (staff's own Claude Code on the model route) has one user, Deyao (relay_tokens: two
    tokens, both deyaochen@yyc.hk), so it may be disrupted: it is not part of the switch's guarantees and stops when
    the old stack retires (A5/A6 remove relay: true, relay.tf and the YYC_AI_RELAY_* keys). WP-58 no longer gates A3;
    it moves to "After this plan", built only if Deyao asks. No one is notified.

15. (Deyao, 2026-11-04) **No MCP: Claude Code reaches tools through REST APIs with `curl`, taught by managed skills.**
    This supersedes every row of DESIGN, the specs and the work packages that names `/mcp`, `managed-mcp.json`,
    `mcp__tools__*` or a "route MCP server" for the sandbox. The new contract:
    - `tools` serves REST (jarvis3 `chat/src/tools/server.ts`, done 2026-11-04): `GET /v1/tools` (name, title,
      description, inputSchema), `POST /v1/tools/<name>` (JSON arguments; the tool's text as `text/plain`, else its
      JSON; 400 bad arguments, 422 the tool's own error, 502 a failure), `GET /v1/openapi.json`. The gateway's fixed
      name `tools` routes `GET /v1/tools`, `POST /v1/tools/<name>` and `GET /v1/openapi.json` (no longer `POST /mcp`);
      the pair bearer and the `Aviary-Sandbox-*` headers are unchanged.
    - Upstreams are REST APIs described by OpenAPI (`<url>/openapi.json`, `POST <url>/<tool>`): yyc-mcp's `/api/v1`.
      yyc-mcp has no MCP endpoint and no OAuth server any more.
    - The supervisor writes no `managed-mcp.json`; it writes the deployment's **managed skills**
      (`/etc/claude-code/.claude/skills/<skill>/<file>`, root, 0644) from hermit's descriptor (a new
      `managedSkills: {"<skill>/<file>": text}` beside `managedClaudeMd`, from hermit's config, part of the etag).
    - The label store is a REST API (`/api/v1`) the sandbox calls through a fixed name with credential injection
      (WP-61), not an MCP server.
    - The old stack runs this today (jarvis3 adcd983: the old stack's tools behind its egress-token front door,
      skills from ConfigMap `jarvis-skills`); YYC's skills are `yyc/deploy/skills/` (their addresses differ on the
      new stack: `http://tools/v1/tools/<name>` through the proxy, no egress token, which WP-62 settles).
    WP-62 builds it on the new stack. (Restored 2026-11-04: commit 8b6caaa1 dropped this entry by accident while
    adding decision 16.) The skills reach the new stack through hermit's descriptor (`managedSkills`), not an image;
    the label store's address is a configured route (platform 3.11.8).

16. (Deyao, 2026-11-04) Claude carries out the full migration. Approved spend: Postgres HA on the new cluster when it
    costs under EUR 20/month; the two bridge load balancers for the switch window (EUR 16.78/month while they run; Hetzner refused them for the
    full primary IP limit, so one bridge server, yyc-bridge1, EUR 7.188/month, carries both: DESIGN 15.5).
    The old Claude Artifact copy of the Talos plan stays. Nothing destructive or irreversible without asking Deyao
    first (Discord ping); reversible steps need no ask: deleting a server after taking its snapshot, releasing a
    stateless pod.
17. (Deyao, 2026-11-05; replaced by 19) **A question nobody answers never holds a sandbox, and the person never sees it time out.**
    Freezing a sandbox (gVisor/runsc checkpoint or pause) to wait for an answer is the wrong layer and is not used in
    any case: YYC sandboxes stay disposable (hermit 3.6.4: pause/resume/snapshot answer 501). Instead, while a run waits
    only on an open question (AskUserQuestion, ExitPlanMode; no tool or background task running), the supervisor
    agrees to the idle release like any idle run; the question stays in magpie with its `tool_use_id` and stays on the
    person's screen. When the person answers, magpie gets a new sandbox, Claude Code resumes the conversation and the
    supervisor delivers the answer to that question (or, if a resumed Claude Code cannot take it, as the next message,
    the same to the person). The time spent waiting for the answer counts in no turn timer: not the displayed working
    time, not the supervisor's silence ceiling, not a turn limit. Both stacks (Deyao, 2026-11-05: the live 30-minute
    patch disarmed, "make them use the new mechanism"): the old stack parks too (claudecodeui fd1b3ed5: the question
    kept in CloudCLI's `jarvis_open_questions`, the answer delivered as the next message, since a resumed Claude Code
    ignores a late tool result), and the importer carries an unanswered question into magpie as an open question by its
    `tool_use_id`. The ask timeout is a 7-day safety limit on both.
18. (Deyao, 2026-11-07) **"Stop waiting": P4 has no soak.** The 72 h soak after A5 was the plan's own margin, not his
    rule (a permission is not a restriction). P4's checks are read once after A5, then `cutover import-done` and A6
    follow; `import-done` has no `--now` flag and no time check (spec-cutover D11, 10.4).
19. (Deyao, 2026-11-17; replaces 17) **A question is a command that blocks until the person answers.** The model asks
    with the `ask` shell command (aviary 80420311): it opens the question with magpie through the supervisor's
    agent.sock and long-polls until the answer, then prints it. The question belongs to the command, not to the turn:
    it shows in `Conversation.asks` (tool `ask`) with or without a turn, and only the person's answer resolves it;
    nothing in the harness or the stack dismisses it. A cancelled or backgrounded command leaves it open (`ask wait
    ID`, `ask status ID`). A message the person sends meanwhile is delivered as during any long command. The
    harnesses' own question tools (`AskUserQuestion`, `ask_user_question`, plan mode's `ExitPlanMode`) are not
    offered; one that still asks is denied with a pointer to `ask`. The minimal rule replaces 17's machinery (the
    supervisor's relay, the `asking` state, parked turns): a run waiting on `ask` is busy like any running command,
    so its sandbox stays while the question waits (17's "never holds a sandbox" no longer holds), and each question
    poll counts as the run's output, so a held run is not silent. magpie's parked turns and the old sandbox routes
    stay only until no sandbox of the previous image is left (aviary 746d247f). Same in Jarvis 4, whose time machine
    can freeze a session that waits on an answer.
