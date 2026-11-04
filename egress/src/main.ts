// Entry point of the chat image: the first argument picks the component (`node src/main.ts egress`, Kubernetes
// `args: [egress]`; docs/CHAT.md, "Components").
//
// Until A6 (aviary refactor DESIGN 10) the old stack runs web from deploy/chat with JARVIS_ROLE and no argument, so
// with no argument JARVIS_ROLE still picks web, and JARVIS_ROLE=tools the old stack's tools (tools/legacy.ts: the same
// tools behind its egress-token front door). Not egress: at this commit it is aviary's sandbox gateway, which the old
// stack cannot run; deploy/chat pins the old stack's egress to the last image of the old contract (docs/CHAT.md
// "Kustomize base").
const arg = process.argv[2];
const legacy = arg === undefined ? process.env.JARVIS_ROLE : undefined;
const role = arg ?? (legacy === "web" ? legacy : legacy === "tools" ? "tools-legacy" : undefined);
switch (role) {
  case "web":
    await (await import("./web/main.ts")).main();
    break;
  case "egress":
    // `egress import-jarvis [--delta]`: the one-off copy of the old stack's egress_log (CronJob egress-import, A2-A6)
    if (process.argv[3] === "import-jarvis") await (await import("./egress/importjarvis.ts")).importJarvis(process.argv.slice(4));
    else await (await import("./egress/main.ts")).main();
    break;
  case "tools":
    // `tools import-jarvis [--delta]`: the one-off copy of the old stack's tool_calls (CronJob tools-import, A2 and A6)
    if (process.argv[3] === "import-jarvis") await (await import("./tools/importjarvis.ts")).importJarvis(process.argv.slice(4));
    else await (await import("./tools/main.ts")).main();
    break;
  case "tools-legacy":
    // the old stack's tools (deploy/chat/tools.yaml, JARVIS_ROLE=tools): the same tools behind its egress-token front door
    await (await import("./tools/legacy.ts")).main();
    break;
  default:
    if (legacy === "egress")
      console.error("JARVIS_ROLE=egress: this image's egress is aviary's sandbox gateway (`args: [egress]`, /etc/egress/egress.json); the old stack's egress runs the image pinned in deploy/chat/egress.yaml");
    else console.error(`the first argument must be web, egress or tools (got ${JSON.stringify(arg ?? null)})`);
    process.exit(2);
}
