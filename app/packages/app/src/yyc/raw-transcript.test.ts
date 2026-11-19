import { describe, expect, it } from "vitest";
import { rawTranscriptHref } from "./raw-transcript";

describe("raw transcript link", () => {
  it("opens pigeon's raw view of the conversation and comes back to this page", () => {
    expect(rawTranscriptHref("0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b", "/h/srv/workspace/wks_0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b?x=1")).toBe(
      "/raw/0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b?back=%2Fh%2Fsrv%2Fworkspace%2Fwks_0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b%3Fx%3D1",
    );
  });
});
