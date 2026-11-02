// Entry point of the chat image: the first argument picks the component (`node src/main.ts egress`, Kubernetes
// `args: [egress]`; docs/CHAT.md, "Components").
const role = process.argv[2];
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
    console.error(`the first argument must be web, egress or tools (got ${JSON.stringify(role ?? null)})`);
    process.exit(2);
}
