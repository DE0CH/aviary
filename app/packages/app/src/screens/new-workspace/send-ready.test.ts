import { describe, expect, it } from "vitest";
import { isNewWorkspaceSendReady } from "./send-ready";

const ready = {
  clientReady: true,
  hasHydratedWorkspaces: true,
  hasProjectOptions: true,
  hasSelectedSourceDirectory: true,
};

describe("isNewWorkspaceSendReady", () => {
  it("sends once connected with the projects in and one chosen", () => {
    expect(isNewWorkspaceSendReady(ready)).toBe(true);
  });

  it("waits for the connection", () => {
    expect(isNewWorkspaceSendReady({ ...ready, clientReady: false })).toBe(
      false
    );
  });

  it("waits for the workspace list after the connection opens", () => {
    expect(
      isNewWorkspaceSendReady({
        ...ready,
        hasHydratedWorkspaces: false,
        hasProjectOptions: false,
        hasSelectedSourceDirectory: false,
      })
    ).toBe(false);
  });

  it("waits while projects exist but none is chosen yet", () => {
    expect(
      isNewWorkspaceSendReady({ ...ready, hasSelectedSourceDirectory: false })
    ).toBe(false);
  });

  it("lets a host with no project send, so its own error explains", () => {
    expect(
      isNewWorkspaceSendReady({
        ...ready,
        hasProjectOptions: false,
        hasSelectedSourceDirectory: false,
      })
    ).toBe(true);
  });
});
