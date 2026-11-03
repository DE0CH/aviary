# testing/e2e: the integration harness (platform 7)

The real turn path on one machine: the app protocol into pigeon, magpie, hermit, the supervisor in the real sandbox
image, Claude Code 2.1.304, the reference gateway and mock-model, with real Postgres, a real S3, real JuiceFS for
`/workspace`, real NetworkPolicies and real rollouts. No cloud account, no model cost. ci.yml's `e2e` job runs it on
every push; the same script runs on a laptop with Docker.

```
testing/e2e/run.sh up <pigeon> <magpie> <hermit> <aviary-testing> <hermit-sandbox>   # images by local tag
testing/e2e/run.sh test [e1 e2 …]                                                      # the driver Job
testing/e2e/run.sh artifacts <dir>                                                     # logs, events
testing/e2e/run.sh down
```

Needs docker, k3d 5.8.3, kubectl, jq, openssl, juicefs 1.4.1 and sudo (the FUSE mounts). Every wait has a bound.
`E2E_PRELOAD=1` copies the cluster's own images (Calico's, k3s's) from the local Docker into the node, for a
machine whose containers cannot pull (an IPv6-only build host).

## What runs where

| Piece | Where | Notes |
|---|---|---|
| Postgres 17.11 (the production image, TLS, scram) | container `aviary-e2e-pg` on Docker network `aviary-e2e` (172.30.0.10), host port 15432 | databases and roles `magpie`, `hermit`, `jfs` |
| S3 (versitygw 1.9.0) | container `aviary-e2e-s3` (172.30.0.11), host port 19000 | buckets `magpie`, `jfs` (MinIO publishes no image any more, `.github/ci/s3.sh`) |
| JuiceFS | two host mounts under `/mnt/aviary` (rshared, shared with the node) | `jfs` with `root-squash=1000:1000` (the hosts' mount: sandboxes' `/workspace`), `jfs-hermit` with `all-squash=1000:1000` (hermit's `/jfs`), as hermit spec 15.4 has them in YYC; `ws/` owned by 1000 (YYC's jfs-owner Job) |
| k3d cluster `aviary-e2e` | rancher/k3s:v1.36.5-k3s1, one server | Calico (NetworkPolicy, and Pods at fixed addresses), traefik off |
| pigeon x2, magpie x2, hermit x2, test-gateway x2 | Deployments of the images under test | env is secrets only; `SSL_CERT_FILE` (the harness CA) for pigeon (fake-access) and hermit (the stand-in EC2) |
| mock-model, fake-tools, fake-access | Deployments of `aviary-testing` | |
| `fake-sandboxd ec2` | Deployment `fake-ec2`, Service `fake-ec2:443` | hermit's EC2 client resolves `ec2.ap-east-1.amazonaws.com` to it (CoreDNS override) |
| `fake-sandboxd host` | the gateway (Deployment `fabric-gateway` at 10.52.0.1) and one Pod per launched host at the address hermit gave it | sandboxd's API on mutual TLS with the fabric PKI (`fabric/host/make-certs.sh`) |
| sandboxes | Pods of `hermit-sandbox` at the addresses hermit chose (10.52.128.0/17) | privileged (FUSE, Docker), `/workspace` from the hosts' mount, no DNS |
| driver | Job `driver` of `aviary-testing` (`aviary-e2e`) | Paseo over `ws://pigeon/paseo-ws`, magpie's and hermit's admin routes, the Kubernetes API for probes |

`gen.sh` makes every credential per run: pair tokens, hermit's token keys, the fabric PKI, the harness CA with
fake-access's and EC2's certificates, the Access RSA keys, `hermit.json` with the run's image digest.

## Where this differs from platform 7 (and why)

- **Calico, not k3s's NetworkPolicy controller.** hermit picks every sandbox's and host's overlay address and signs
  the sandbox token for it; the gateway checks the token's address against the connection's (3.11.2). Pods must
  therefore have the addresses hermit chose, which Calico gives (`cni.projectcalico.org/ipAddrs`, from an IP pool
  that assigns nothing by itself). Calico enforces the same NetworkPolicies. The cluster CIDR is 10.48.0.0/13 so that
  traffic from the fabric's 10.52.0.0/16 is not masqueraded by kube-proxy.
- **One node**, not one server and two agents: every fixed address is routed on the node itself, with no
  encapsulation between Docker containers.
- **Hosts come from a stand-in EC2, not `maxHosts: 0`.** hermit places sandboxes only on AWS hosts, never on the
  gateway, and makes hosts only through `RunInstances`; with `maxHosts: 0` nothing could run. `fake-sandboxd ec2`
  answers EC2's query protocol and starts a host Pod from the user data hermit sends (`maxHosts: 1`). Migration
  routes answer 501, so packing never moves a sandbox here; hermit's own tests and the staging acceptance cover that.
- **Sizes.** Under runc the supervisor's memory guard reads the node's `/proc/meminfo` (gVisor shows each sandbox its
  own), so a sandbox's limit must stay above what the whole node uses: `sandbox.memory` is 28Gi on a stand-in host
  of 256 GiB. fake-sandboxd sets the spec's sizes as Pod limits with small requests, so the one node schedules what
  hermit packs. The guard itself is tested in the supervisor's tests and on the fabric.
- **hermit.json's `sandbox.env` carries the proxy variables.** Claude Code gets the descriptor's `env` and nothing of
  the boot environment beyond `PATH` and the locale (supervisor 6.2), so without `HTTP_PROXY` and its spellings in
  `sandbox.env` it tries the fixed names by DNS and fails with `EAI_AGAIN`. YYC's `hermit/hermit.json` (WP-31) needs
  the same four entries.

## Scenarios

`src/scenarios.rs`, in the order of `ALL`: E1 (sign-in, the second JWKS key), E2 (a text turn of 40 chunks: whole
rows, dense row seqs, the end after the last row, the generated title), E3 (a Bash tool turn writes
`/workspace/a.txt`: it downloads byte-equal and is on the JuiceFS mount), E4 (`SendUserFile` of 5 MiB: the sent-file
snapshot downloads byte-equal after the workspace file is gone), E5 (a 6000×4000 EXIF-rotated JPEG: the model gets
one image, rotated, long edge at most 2000 px and at most 3,932,160 bytes; the thumbnail loads signed and 404s with a
bad signature), E19 (from a sandbox, magpie by name, by
Service and by Pod, hermit's 8080 and the internet all fail without the proxy; the proxy answers), E16 (the ops page
admin-only through pigeon, hermit's snapshot for the monitor), E14 (delete, restore, delete for good: the other
socket's list push, magpie 404, the sandbox released and `ws/<id>` gone), and `cc`: magpie 20's Claude Code
acceptance on the real binary (WP-12's carried item): person turns with a foreground subagent that runs its own Bash
call, `/compact` with `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1` (in hermit.json's `sandbox.env`), and a turn after it;
every turn completes, the compaction and the subagent's transcript and meta file are in magpie, no line has two
events, every user or assistant line that is not a prompt's echo (a slash command's `<command-name>` form included)
has its event, and the subagent's lines made none. The tombstone stays unreproduced by 2.1.304 (supervisor GATE.md
open item 3). Then E6 (120 turns, then `tail` and `before` pages of 40: no gap, no repeat, no page slower than
twice the first), E7 (the socket closed at the first live row and reopened: the catch-up after the last seen seq and
the live rows end every row where the timeline ends it), E8 (Stop during a 60 s stream: canceled at once, the next turn works), E10 (`kill -9` of the
supervisor mid-turn: the turn completes, every transcript line once in magpie), E11 (the sandbox Pod deleted
mid-turn: `sandbox.lost` within 40 s, the next turn on a new sandbox sees the history), E18 (the same requests through
each pigeon Pod answer alike), E9 (rollouts of pigeon, magpie, hermit and the gateway one after the other during a
90 s turn: the turn completes, the socket, reopened when pigeon's rollout closes it and resumed after its last row,
sees every row once), E21 (the socket closed during `/compact` and reopened: one catch-up after the last row, no reset, the
epoch unchanged, every row seen at its last seq), E23 (both gateway Pods get SIGTERM during a 60 s model stream: it completes, and
`drain_cut_total` stays 0 on the draining Pods; the harness's gateway grace is 620 s, above the model drain bound), E13 (a share seen by another person stops at the share point; continuing it
makes the viewer's conversation with the shared `/workspace`), E15 (view as: an admin's view of Alice lists her
conversations and refuses a rename and a delete; staff cannot start one; the link is refused to anyone else), E24 (hermit at 0 replicas for 3 minutes during a turn with a tool call: the turn
completes), E12 (the idle release, then a new sandbox that resumes the history) and E17 (seven starts with six slots:
never more than six sandboxes, the seventh waits and completes once a pool victim frees a slot; the harness's
`reserveSeconds` and `poolVictimIdleSeconds` are 20 so that happens inside `capacityWaitSeconds`). Last, `stale`: magpie 6.10's stale append end to end (WP-12's carried item),
from inside a sandbox through the gateway: an append, its retry under a new `opId` at the same offset (rule 0: 200,
nothing written twice), a different append at that stale offset (409 `offset_mismatch`), the append at the size, and
magpie's bytes read back. It takes the conversation's lease, so it runs last. The other scenarios of 7.8 are still
to come (WP-28's remaining list).

Found by the harness, for other packages: right after hermit returns from an outage longer than `hostDeadSeconds`, a
start fails `hosts_full` until the hosts' next heartbeat (every host looks dead); E24 waits 20 s before the next
scenario.
