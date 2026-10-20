// Entry point of the chat image: JARVIS_ROLE picks the component (docs/CHAT.md, "Components").
const role = process.env.JARVIS_ROLE;
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
    console.error(`JARVIS_ROLE must be web, egress or tools (got ${JSON.stringify(role)})`);
    process.exit(2);
}
