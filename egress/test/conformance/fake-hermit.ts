// chat.yml conformance, the model suite (aviary platform spec 8.3, 17.3): the suite brings no hermit of its own, so this
// serves egress the two hermit routes it calls (the token key set and liveness, every sandbox live) and writes one
// sandbox token, valid from any address, for `aviary-conformance model --sandbox-token`.
// Usage: node fake-hermit.ts <listen port> <gateway→hermit bearer> <token out file>
import { writeFileSync } from "node:fs";
import { FakeHermit } from "../fakes/hermit.ts";

const [port, bearer, out] = process.argv.slice(2);
if (!port || !bearer || !out) {
  console.error("usage: fake-hermit.ts <port> <bearer> <token file>");
  process.exit(2);
}
const h = new FakeHermit(bearer);
await h.listen(Number(port), "0.0.0.0");
writeFileSync(out, h.token({ source: "0.0.0.0", person: "conformance@example.org" }).token + "\n");
console.log(JSON.stringify({ at: "fake hermit listening", port: Number(port) }));
process.on("SIGTERM", () => {
  h.close();
  process.exit(0);
});
