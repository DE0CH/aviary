# Gateway node maintenance

The sandbox gateway (sandboxd in gateway mode: the WireGuard hub, the egress VIP's DNAT, the route into the overlay)
runs as the `hermit-gateway` DaemonSet pod on the one Kubernetes node labelled `hermit.yyc/gateway=true`
(`gateway-net.sh` sets the label, the network route and checks the firewall). While that node is down every sandbox
has lost its egress and control path: running turns stall on model calls and FUSE writes, new starts wait and end
`sandbox_busy` after `hostWaitSeconds`. A reboot, an OS upgrade or a replacement of that node is therefore not
zero-downtime for running turns (hermit spec 14.7, the stated gap). This runbook keeps it small and orderly.

A restart of the gateway **pod** (a sandboxd upgrade, a DaemonSet update) is not node work and needs no runbook:
sandboxd leaves `wg0`, its nft table, rules and routes in place and adopts them at start (hermit 14.2).

## When

- The deployment's routine node work (e.g. a weekly OS upgrade workflow) skips the gateway node and upgrades it only
  through this runbook; anything that restarts the gateway node's kubelet (a Kubernetes upgrade) likewise.
- A provider maintenance notice for the gateway's server: run the runbook at a chosen quiet time, with the node work
  being that reboot.
- Never while the deployment's node-work hold is set (below): the node-work command itself skips then, and the runbook
  has nothing to do.

## The script

```
HERMIT_ADMIN_TOKEN=… HERMIT_ADMIN=<operator email> HERMIT_URL=http://127.0.0.1:18080 \
  crates/hermit/scripts/hermit-admin.sh gateway-maintenance --node-work "<command>" \
    [--deadline 30] [--back 30] [--settle 30]
```

Reach hermit first (e.g. `kubectl -n <namespace> port-forward svc/hermit 18080:80`); the admin pair token goes in
`HERMIT_ADMIN_TOKEN`, never on the command line. Steps (hermit 14.7):

1. `PUT /v1/admin/sandboxes-enabled {"enabled": false, "reason": "gateway maintenance"}`: new starts answer
   `sandbox_busy` (`disabled`) at once; running sandboxes are untouched.
2. Poll `GET /ops/snapshot` (only snapshots built after step 1; hermit caches one for 4 s) until no sandbox is busy
   (`sandboxes.list[].busy` all false; a failed section counts as busy), at most `--deadline` minutes (default 30);
   then go on anyway and say how many were busy.
3. Run `<command>` with bash. **It must block until the node work is finished** (a dispatch that returns at once
   would let step 5 re-enable sandboxes before the node goes down) and exit non-zero when it failed. A failure is
   reported; steps 4 and 5 still run, since the node may be back either way.
4. Poll the snapshot (only snapshots built after the node work ended) until the gateway's host row is reachable
   (`hosts.list[]` with `gateway: true, reachable: true`, i.e. hermit has its heartbeat) and egress targets are pushed
   (`fabricOps.egressTargets > 0`), both for `--settle` seconds in a row (default 30), at most `--back` minutes
   (default 30). If that never happens, new sandboxes stay
   **off** and the script exits 1: fix the gateway (`kubectl -n yyc-ai-fabric get pod -o wide`, the node's state),
   then `hermit-admin.sh enable`.
5. `PUT /v1/admin/sandboxes-enabled {"enabled": true}`.

Exit 0 only when the node work succeeded and sandboxes are on again; the node work's own exit code when it failed
but the gateway came back. Interrupting the script (Ctrl-C) leaves new sandboxes as they are (off after step 1):
`hermit-admin.sh enable` once the gateway answers. Every switch is in the admin audit log (`hermit-admin.sh events`,
action `sandboxes.enabled`).

While the gateway is away hermit decides nothing about sandboxes: AWS hosts are not judged lost, the silence rule
pauses until the gateway has answered for `gatewaySettleSeconds`, pending soft releases wait. Idle sandboxes and their
held background work survive a reboot of a minute or two.

## The node-work command of a deployment

The command is the deployment's, e.g. YYC's `news-stream/deploy/clusters/talos/gateway-node-work.sh`
(`[--k8s VERSION]`): it dispatches the Talos node-work workflow for the gateway node with `gateway_runbook=true` and
waits for that run, exiting with its result. Any blocking command works for a test or a manual reboot, e.g.
`talosctl -n <gw> reboot --wait` with the right credentials.

## The node-work hold (file format)

A deployment's node-work tooling (OS upgrades, Kubernetes upgrades, a descheduler) reads one JSON file from its
repository's main branch before it starts and again before every node; YYC's is `yyc-ai/cutover/window.json`:

```json
{"nodeWork": "hold"}
```

- `"nodeWork": "hold"`: no node of either cluster is drained, upgraded or rebooted, the gateway node included; the
  run ends green having done nothing. The file may carry other keys (the cutover window's), which node work ignores.
- The key absent, any other value, or no file: node work may run (the gateway node still only through this runbook).
- The branch cannot be read: node work stops red (never work blind).

The cutover runner's `preflight` sets the hold for the switch window and the window's last step clears it (YYC
DESIGN 15.2 N8).

## Tested

`crates/hermit/tests/ops.rs` `the_gateway_maintenance_runbook_holds_the_node_work_and_the_restart` drives the script
against a hermit replica in the harness with a fake node-work command: sandboxes go off, the node work waits while a
sandbox is busy, sandboxes stay off while the gateway's heartbeat is stale and egress targets are gone, and come back
on once both return; a failed node work is reported with its exit code; a gateway that never returns leaves them off
and exits 1.
