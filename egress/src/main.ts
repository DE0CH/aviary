// Entry point of the chat image: the first argument picks the component (`node src/main.ts egress`, Kubernetes
// `args: [egress]`; docs/CHAT.md, "Components").
//
// Until A6 (aviary refactor DESIGN 10) the old stack runs web and tools from deploy/chat with JARVIS_ROLE and no
// argument, so with no argument JARVIS_ROLE still picks web or tools: an old-stack bump of the jarvis3 pin keeps
// working for them. Not for egress: egress at this commit is aviary's sandbox gateway, which the old stack cannot
// run; deploy/chat pins the old stack's egress to the last image of the old contract (docs/CHAT.md "Kustomize base").
const arg = process.argv[2];
const legacy = arg === undefined ? process.env.JARVIS_ROLE : undefined;
const role = arg ?? (legacy === "web" || legacy === "tools" ? legacy : undefined);
switch (role) {
  case "web":
    await (await import("./web/main.ts")).main();
    break;
  case "egress":
    await (await import("./egress/main.ts")).main();
    break;
  case "tools":
    await (await import("./tools/main.ts")).main();
    break;
  default:
    if (legacy === "egress")
      console.error("JARVIS_ROLE=egress: this image's egress is aviary's sandbox gateway (`args: [egress]`, /etc/egress/egress.json); the old stack's egress runs the image pinned in deploy/chat/egress.yaml");
    else console.error(`the first argument must be web, egress or tools (got ${JSON.stringify(arg ?? null)})`);
    process.exit(2);
}
