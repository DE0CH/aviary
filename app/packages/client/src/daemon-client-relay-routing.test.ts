import { PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE as RELAY_CODE } from "@getpaseo/relay/e2ee";
import { describe, expect, it } from "vitest";
import { PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE } from "./daemon-client-relay-routing.js";

describe("relay routing", () => {
  it("knows the relay's protected-handshake close code without loading the encryption", () => {
    expect(PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE).toBe(RELAY_CODE);
  });
});
