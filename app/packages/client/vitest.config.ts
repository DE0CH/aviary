import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // The server-message validator loads as its own chunk (daemon-client.ts loadWSOutboundValidator); tests that
    // expect connect() to open the socket synchronously need it loaded first.
    setupFiles: ["./src/test-setup.ts"],
  },
});
