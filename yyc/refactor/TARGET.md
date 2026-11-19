# Target design: pigeon, magpie, hermit, supervisor (Deyao, 2026-11-02)

Rules
- Services talk to each other only over REST; each owns its database behind its API.
- No component has deployment-specific configuration or behaviour switches. Every component reaches
  the others at fixed endpoint names; a deployment differs only in what implements those endpoints.
  Dogfooding = the same builds, unmodified, in every deployment.
- "agent" = Claude Code and the model's turn.
- Names are used everywhere in code and deployment.
- Each deployment (YYC AI, Jarvis 4) runs its own instances; separate infra.

Endpoints a sandbox sees (fixed names, all reached through the sandbox gateway):
  hermit  - lifecycle, secrets
  magpie  - conversation state
  model   - the Anthropic Messages API; credentials live behind it, never in the sandbox
  tools   - MCP
  internet - whatever the gateway lets through

pigeon - stateless gateway, autoscaled. Sign-in, Paseo protocol to the web and iOS apps; asks hermit
  who may sign in. No per-person processes, no conversation state; any replica serves anyone.
magpie - owns conversations, autoscaled replicas. Conversation list, titles, shares, deletions, events,
  Claude Code's state, files panel, sent-file snapshots. Events = rows in its own Postgres, seq per
  conversation. Images and large tool output stored once in the bucket; row = reference +
  thumbnail/preview; full content fetched on tap. Live fan-out between replicas inside magpie.
hermit - the contract: sandbox lifecycle (get/start for a conversation, warm, release, pause, resume,
  snapshot), who may use the deployment, and per-sandbox secrets by key (an empty grant is a valid
  answer). Implementations: YYC (gVisor on AWS, JuiceFS /workspace, no secrets granted); Jarvis 4
  (Fly Machines, the kernel and its stores).
sandbox gateway - the sandbox's only network path; serves the fixed endpoint names and decides what
  internet traffic passes and how it is logged. Implementations: YYC egress (allowlist, threat DNS,
  upload cap, model = OpenRouter with ZDR, model list, cost and budget, tools route); Jarvis 4's
  (open internet, model = Claude subscription).
supervisor - one long-running process per sandbox that owns it; OOM-protected, restarted by a keeper.
  Runs the agent detached. Always does the same things the same way: presents Claude Code's state
  (~/.claude) as files through FUSE backed by magpie (a write returns once magpie has stored it);
  points Claude Code at `model` and `tools`; hands out secrets per key by asking hermit; answers
  hermit's lifecycle calls (pause, snapshot, release). /workspace is a mount hermit provides.
tools - read-only MCP behind the `tools` endpoint.

Turn: app -> pigeon (socket) -> POST magpie /conversations/{id}/turns -> magpie gets a sandbox from
hermit, tells its supervisor to start the turn -> Claude Code writes its transcript through FUSE ->
the supervisor stores each write in magpie (next seq) -> pigeon streams
GET magpie /conversations/{id}/events?after=<seq> to the tabs; a reconnect resumes by seq.
History: GET magpie /conversations/{id}/events?before=<seq>&limit=40; cost ~ page size.
Existing conversations: imported into magpie once, before the switch.

Open (Jarvis 4): its client; where Remote Control, the screen/terminal and the other harnesses sit;
whether wakeups, watches and Tasks are hermit's.
