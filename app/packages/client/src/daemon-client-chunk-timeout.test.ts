import { expect, test, vi } from "vitest";

// The relay encryption chunk never arrives (a stalled script request on web)
vi.mock("./daemon-client-relay-e2ee-transport.js", () => new Promise(() => undefined));

const { DaemonClient } = await import("./daemon-client.js");

test("a connect waiting on a stalled chunk fails at the connect timeout instead of hanging", async () => {
  const transportFactory = vi.fn();
  const client = new DaemonClient({
    url: "ws://relay.test/ws?role=client&serverId=srv_test&v=2",
    clientId: "clsk_chunk_timeout_test",
    e2ee: { enabled: true, daemonPublicKeyB64: "daemon-public-key" },
    reconnect: { enabled: false },
    connectTimeoutMs: 50,
    transportFactory,
  });
  try {
    await expect(client.connect()).rejects.toThrow("Connection timed out");
    expect(transportFactory).not.toHaveBeenCalled();
  } finally {
    await client.close().catch(() => undefined);
  }
});
