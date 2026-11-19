// safeGet follows a Location header written in raw UTF-8 (星島's stheadline.com does this) instead of looping.
import assert from "node:assert/strict";
import { createServer } from "node:net";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import { locationHeader, safeGet } from "../src/tools/netguard.ts";

test("locationHeader: raw UTF-8 read as Latin-1 comes back as UTF-8; ASCII and real Latin-1 stay", () => {
  const path = "/3635149/港產手術機械人";
  assert.equal(locationHeader(Buffer.from(path, "utf8").toString("latin1")), path);
  assert.equal(locationHeader("/a%20b?c=d"), "/a%20b?c=d");
  assert.equal(locationHeader("/caf\xe9"), "/caf\xe9");
});

test("safeGet: a redirect to a raw UTF-8 path is followed once, not until too many redirects", async () => {
  const target = "/3635149/港產手術機械人";
  const srv = createServer((sock) => {
    sock.once("data", (d) => {
      const line = d.toString("latin1").split("\r\n")[0]!;
      const want = `GET ${encodeURI(target)} `;
      const head = line.startsWith(want)
        ? Buffer.from("HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
        : Buffer.concat([Buffer.from("HTTP/1.1 302 Found\r\nlocation: "), Buffer.from(target, "utf8"), Buffer.from("\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")]);
      sock.end(head);
    });
  });
  await new Promise<void>((ok) => srv.listen(0, "127.0.0.1", ok));
  const port = (srv.address() as AddressInfo).port;
  try {
    const page = await safeGet(`http://fake.test:${port}/3635149/`, { resolve: async () => ({ address: "127.0.0.1", family: 4 }), ports: "any" });
    assert.equal(page.status, 200);
    assert.equal(page.body.toString(), "ok");
    assert.equal(decodeURI(new URL(page.url).pathname), target);
  } finally {
    srv.close();
  }
});
