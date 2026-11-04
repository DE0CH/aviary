# testing/lu-rig: the LU rig against the real old stack (WP-54)

`crates/pigeon/tests/legacy.rs` proves the switch window's legacy upstream (LU) against a fake old router. This rig
runs pigeon's LU against the real one: the ui image of the A1 commit (the router with the switch module, WP-56, and
CloudCLI) running real Claude Code turns, beside a stand-in of its web role and sandbox agent and a mock model.

```
testing/lu-rig/run.sh up ghcr.io/de0ch/yyc-ai-ui:<sha12>   # Postgres, mock model, stand-in, ui; then one smoke turn
testing/lu-rig/run.sh test                                  # cargo test -p pigeon --test lu_rig -- --ignored
testing/lu-rig/run.sh logs <dir>                            # container logs, the stand-in's calls, the model's requests
testing/lu-rig/run.sh down
```

Needs docker, openssl and curl. `nightly.yml` runs it on the ui image pinned in `ui-image.txt`.

| Piece | What |
|---|---|
| `lu-ui` | the ui image unchanged, run as `jarvis/ui.yaml` runs it: config with `access.jwksFile` (the rig's key), the switch module on (`JARVIS_UI_SWITCH_TOKEN`, `JARVIS_UI_LU_TOKEN` = pigeon's harness pair token, pass-on to a closed port), the purge off |
| `lu-old` (`old/fake-web.mjs`) | jarvis3 `web`'s `/internal/cc/*` and the sandbox agent's `GET /proc`, in a container of the ui image (its Claude Code binary): each process in its own mount namespace with `/workspace` = `/jfs/ws/<conversation>` and `~/.claude` = the person's `/jfs/users/<key>/claude`, as uid 1000; a `/proc` that drops while its process runs is logged as a cut (the old agent kills Claude Code then) |
| `lu-model` (`old/mock-model.mjs`) | the Messages API: `[[slow N]]` is N Bash calls (`sleep 1; echo chunk k`) then `done N`; anything else is `Echo: …`; `/__log` keeps what each request's last user message was |
| `lu-pg` | Postgres 17 (database `cloudcli`) |

The cases (`crates/pigeon/tests/lu_rig.rs`, pigeon in-process before the in-memory magpie model, its LU through a
relay the cases can reset): a send runs once on the old child and streams back through LU under the tab's
subscription, with a Send ticket closed on `started` and the old router counting the socket as LU (a move closes
nothing); a running old turn streams across a move and Stop ends it under a Stop ticket; an LU reset under a live tab
is reopened (E4) and the old turn runs to its end; a teed upload lands in the old child's `cloudcli-assets/` and
reaches the agent's prompt; an old-router 401 is pigeon's signed-out answer (4401) with no ticket. Each case checks
pigeon's zero counters and that no old turn was cut.
