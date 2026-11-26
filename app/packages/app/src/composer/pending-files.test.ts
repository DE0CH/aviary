import { describe, expect, it } from "vitest";
import type { SelectedFile } from "@/attachments/selected-file";
import {
  withPendingFileProgress,
  withoutPendingFiles,
  type PendingFileAttachment,
} from "./pending-files";

function file(fileName: string): SelectedFile {
  return { fileName, mimeType: "text/plain", readBytes: async () => new Uint8Array() } as SelectedFile;
}

describe("pending file chips", () => {
  it("an upload that reported progress leaves no chip once it ends", () => {
    const placeholders: PendingFileAttachment[] = [{ id: 0, file: file("report.pdf") }];
    let pending: PendingFileAttachment[] = [...placeholders];
    pending = withPendingFileProgress(pending, 0, { sent: 512, total: 1024 });
    pending = withPendingFileProgress(pending, 0, { sent: 1024, total: 1024 });

    expect(withoutPendingFiles(pending, placeholders)).toEqual([]);
  });

  it("keeps the chips of another upload still running", () => {
    const first: PendingFileAttachment[] = [{ id: 0, file: file("a.txt") }];
    const second: PendingFileAttachment[] = [{ id: 1, file: file("b.txt") }];
    let pending: PendingFileAttachment[] = [...first, ...second];
    pending = withPendingFileProgress(pending, 0, { sent: 10, total: 10 });
    pending = withPendingFileProgress(pending, 1, { sent: 3, total: 10 });

    expect(withoutPendingFiles(pending, first)).toEqual([
      { id: 1, file: second[0]!.file, progress: { sent: 3, total: 10 } },
    ]);
  });
});
