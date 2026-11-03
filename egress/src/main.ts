// Entry point of the chat image: the first argument picks the component (`node src/main.ts egress`, Kubernetes
// `args: [egress]`; docs/CHAT.md, "Components").
//
// Until A6 (aviary refactor DESIGN 10) the old stack runs web from deploy/chat with JARVIS_ROLE and no argument, so
// with no argument JARVIS_ROLE still picks web: an old-stack bump of the jarvis3 pin keeps working for it. Not for
// egress and tools: at this commit they are aviary's sandbox gateway and the tools of its contract, which the old
// stack cannot run; deploy/chat pins the old stack's egress and tools to the last image of the old contract
// (docs/CHAT.md "Kustomize base").
const arg = process.argv[2];
const legacy = arg === undefined ? process.env.JARVIS_ROLE : undefined;
const role = arg ?? (legacy === "web" ? legacy : undefined);
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
  default:
    if (legacy === "egress")
      console.error("JARVIS_ROLE=egress: this image's egress is aviary's sandbox gateway (`args: [egress]`, /etc/egress/egress.json); the old stack's egress runs the image pinned in deploy/chat/egress.yaml");
    else if (legacy === "tools")
      console.error("JARVIS_ROLE=tools: this image's tools serve aviary's gateway (`args: [tools]`, /etc/tools/tools.json); the old stack's tools run the image pinned in deploy/chat/tools.yaml");
    else console.error(`the first argument must be web, egress or tools (got ${JSON.stringify(arg ?? null)})`);
    process.exit(2);
}
