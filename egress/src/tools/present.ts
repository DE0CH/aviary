// present_files (docs/CHAT.md, "Presented files"): the agent names the deliverables of its answer, and only those are
// shown to the staff member as download cards under the answer. Read-only: it changes nothing, it only marks files.
// The workspace is on the web replica that runs the turn, so the stdio bridge there (jarvis-mcp-bridge, cwd = the
// workdir) checks each path and passes what it found in the call's `_meta["jarvis/files"]`; this tool trusts only that.
import { tx } from "../common/db.ts";
import { appendEvent } from "../web/events.ts";
import { MAX_CAPTION, MAX_PRESENTED, mergePresented, presentedOf, type PresentedFile } from "../common/presented.ts";
import { text, ToolInputError, type ToolDef } from "./types.ts";

export const FILES_META = "jarvis/files";

/** What the bridge found for one path the agent gave: the workspace-relative path with its size and hash, or why not. */
export type FileFact = { path: string; size: number; sha256: string } | { error: string };

export const presentFiles: ToolDef = {
  name: "present_files",
  title: "Show files to the user",
  description:
    "Show files from the workspace to the user as downloads under your answer (images and PDFs get a preview). Only " +
    "the files you pass here are shown; nothing else in the workspace is visible to the user. When you finish, call " +
    "it once with the deliverables only (reports, charts, data files the user asked for), in the order the user should " +
    "see them, never scripts, fonts or intermediate files. Paths are relative to the workspace (/work in the sandbox). " +
    "Calling it again in the same answer adds to the list; a path given again is updated. Don't also paste download " +
    "links or file paths for these in your text.",
  inputSchema: {
    type: "object",
    properties: {
      files: {
        type: "array",
        minItems: 1,
        maxItems: MAX_PRESENTED,
        items: {
          type: "object",
          properties: {
            path: { type: "string", description: "Workspace-relative path, e.g. report.pdf or out/chart.png." },
            caption: { type: "string", description: "Optional short caption shown with the file." },
          },
          required: ["path"],
          additionalProperties: false,
        },
      },
    },
    required: ["files"],
    additionalProperties: false,
  },
  annotations: { title: "Show files to the user", readOnlyHint: true, openWorldHint: false },
  async run(args, ctx) {
    const list = args.files;
    if (!Array.isArray(list) || !list.length) throw new ToolInputError('"files" must be a non-empty list of {path, caption?}');
    if (list.length > MAX_PRESENTED) throw new ToolInputError(`at most ${MAX_PRESENTED} files at once`);
    const facts = (ctx.meta?.[FILES_META] ?? null) as Record<string, FileFact> | null;
    if (!facts || typeof facts !== "object") return text("present_files works only inside a chat turn (the workspace could not be checked).", true);
    const problems: string[] = [];
    const picked: PresentedFile[] = [];
    for (const item of list) {
      const path = item && typeof item === "object" ? (item as { path?: unknown }).path : undefined;
      const caption = item && typeof item === "object" ? (item as { caption?: unknown }).caption : undefined;
      if (typeof path !== "string" || !path.trim()) throw new ToolInputError("each file needs a path");
      if (caption !== undefined && typeof caption !== "string") throw new ToolInputError(`the caption of ${path} must be a string`);
      const f = facts[path];
      if (!f) problems.push(`${path}: not checked`);
      else if ("error" in f) problems.push(`${path}: ${f.error}`);
      else {
        const c = (caption ?? "").replace(/\s+/g, " ").trim().slice(0, MAX_CAPTION);
        picked.push({ path: f.path, ...(c ? { caption: c } : {}), sha256: f.sha256, size: f.size });
      }
    }
    if (problems.length) {
      return text(`Nothing was shown. Fix these paths and call present_files again (paths are relative to the workspace):\n${problems.map((p) => `- ${p}`).join("\n")}`, true);
    }
    const db = ctx.deps.db;
    // one writer per turn at a time (the agent may call tools in parallel)
    const merged = await tx(db, async (q) => {
      const { rows } = await q.query<{ presented: unknown }>("select presented from turns where id = $1 for update", [ctx.turn.id]);
      const next = mergePresented(presentedOf(rows[0]?.presented) ?? [], picked);
      await q.query("update turns set presented = $2 where id = $1", [ctx.turn.id, JSON.stringify(next)]);
      return next;
    });
    await appendEvent(db, ctx.turn.conversation_id, ctx.turn.id, "files_presented", { files: merged });
    const names = picked.map((f) => f.path).join(", ");
    return {
      content: [{ type: "text", text: `Shown to the user under your answer: ${names}. Don't repeat links or paths for them in your text.` }],
      structuredContent: { files: merged },
    };
  },
};
