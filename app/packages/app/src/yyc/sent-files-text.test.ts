import { describe, expect, it } from "vitest";
import { readSentCaption } from "./sent-files-text";

// The caption of a send shows over its cards. The server puts it on the call's metadata (`sentCaption`), whichever
// tool sent the files; a server that does not send it leaves the SendUserFile call's own `caption` input.
describe("the caption of the files a call sent", () => {
  const files = [{ path: "out/report.pdf", name: "report.pdf", size: 3, mediaType: "application/pdf", isImage: false }];
  const shell = { type: "shell", command: "send-file --caption 'The report' out/report.pdf", output: "sent" };

  it("reads the caption the server put on the call, whichever tool sent the files", () => {
    expect(readSentCaption({ sentFiles: files, sentCaption: " The report " }, shell)).toBe("The report");
    expect(
      readSentCaption(
        { sentFiles: files, sentCaption: "The report" },
        { type: "unknown", input: { path: "out/report.pdf" }, output: "sent" },
      ),
    ).toBe("The report");
  });

  it("reads SendUserFile's caption input when the server sends none", () => {
    const detail = { type: "unknown", input: { path: "out/report.pdf", caption: " As asked " }, output: "sent" };
    expect(readSentCaption({ sentFiles: files }, detail)).toBe("As asked");
    expect(readSentCaption(undefined, detail)).toBe("As asked");
    expect(readSentCaption({ sentFiles: files, sentCaption: "  " }, detail)).toBe("As asked");
  });

  it("has none for a send without a caption", () => {
    expect(readSentCaption({ sentFiles: files }, shell)).toBeNull();
    expect(readSentCaption({ sentFiles: files, sentCaption: 3 }, { type: "unknown", input: null })).toBeNull();
    expect(readSentCaption(undefined, undefined)).toBeNull();
  });
});
