// Entry point of the chat image: the first argument picks the component (`node src/main.ts egress`, Kubernetes
// `args: [egress]`; docs/CHAT.md, "Components").
const arg = process.argv[2];
switch (arg) {
  case "egress":
    await (await import("./egress/main.ts")).main();
    break;
  case "tools":
    await (await import("./tools/main.ts")).main();
    break;
  default:
    console.error(`the first argument must be egress or tools (got ${JSON.stringify(arg ?? null)})`);
    process.exit(2);
}
