/**
 * The readings of sent files (yyc/sent-files.tsx): what a tool call sent to the person, read off the call.
 * No React Native here, so the unit tests run without it.
 */

/** One file sent to the person (the server puts these on the metadata of the tool call that sent them). */
export interface SentFile {
  path: string;
  name: string;
  size: number | null;
  mediaType: string | null;
  isImage: boolean;
  /** The call it came with: the download reads the file as that send kept it. */
  sent?: string;
}

/** The caption Claude gave the files (SendUserFile's `caption` input), if any. */
export function readSendUserFileCaption(detail: unknown): string | null {
  const input = (detail as { input?: unknown } | undefined)?.input;
  const caption = (input as { caption?: unknown } | null)?.caption;
  return typeof caption === "string" && caption.trim() ? caption.trim() : null;
}

export function readSentFiles(metadata: unknown): SentFile[] {
  const files = (metadata as { sentFiles?: unknown } | undefined)?.sentFiles;
  return Array.isArray(files) ? (files as SentFile[]) : [];
}

/**
 * Whether a tool call sent files to the person (its cards show): the SendUserFile call, or whichever call carries
 * sent files (an agent program without that tool sends from a shell command). Such a call is never folded into a
 * group or a turn's summary (tool-calls/detail-level/grouping.ts, agent-stream/turn-collapse).
 */
export function isSentFilesCall(name: string, metadata: unknown): boolean {
  return name.trim().toLowerCase() === "senduserfile" || readSentFiles(metadata).length > 0;
}
