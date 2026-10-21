// The web role's public API (docs/CHAT.md, "Public API"). Same origin, behind Cloudflare Access: the browser's
// CF_Authorization cookie authenticates every call.

export interface Me {
  email: string;
  name?: string | null;
  /** The identity provider's first name (Google sign-in), for the greeting; null otherwise. */
  firstName?: string | null;
  isAdmin: boolean;
  product: { name: string; logoUrl?: string | null; logo?: string | null; locale?: string };
  models: { id: string; label: string }[];
  defaultModel: string;
}

export interface ConversationSummary {
  id: string;
  title: string;
  updatedAt: string;
  running: boolean;
}

export interface ChatEvent {
  id: number;
  turnId: string;
  seq: number;
  type: string;
  data: any;
  createdAt: string;
}

export interface Message {
  id: number;
  role: "user" | "assistant";
  content: string;
  turnId?: string | null;
  attachments?: string[];
  createdAt: string;
}

/** A file the agent showed under its answer (present_files), at the version the turn left. */
export interface PresentedFile {
  path: string;
  caption?: string;
  sha256: string;
  size: number;
}

export interface Turn {
  id: string;
  status: "queued" | "running" | "waiting" | "done" | "error" | "cancelled";
  startedAt?: string;
  finishedAt?: string | null;
  error?: string | null;
  /** what the turn shows as file cards (a turn from before present_files: the files it changed) */
  presented?: PresentedFile[] | null;
  events: ChatEvent[];
}

export interface Conversation {
  id: string;
  title: string;
  model: string;
  messages: Message[];
  turns: Turn[];
}

/** A share link of the caller's conversation. */
export interface ShareLink {
  id: string;
  url: string;
  createdAt: string;
  turns: number;
}

/** A shared conversation as its viewers see it: read-only, frozen when it was shared. */
export interface SharedConversation {
  id: string;
  title: string;
  ownerName: string;
  isOwner: boolean;
  createdAt: string;
  messages: Message[];
  turns: Turn[];
  files: { path: string; size: number; source: "upload" | "agent"; updatedAt?: string }[];
}

/** The id components use for a share page's files: api.fileUrl sends it to the share's own download route. */
export const shareRef = (shareId: string) => `share:${shareId}`;

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function call<T>(method: string, path: string, body?: unknown, retries = 2): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin", headers: {} };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)["content-type"] = "application/json";
  }
  let r: Response;
  try {
    r = await fetch(path, init);
  } catch (e) {
    if (retries > 0) return wait(800).then(() => call<T>(method, path, body, retries - 1));
    throw new ApiError(0, (e as Error).message);
  }
  // A replica that is draining answers 503: another replica takes the request.
  if (r.status === 503 && retries > 0) return wait(500).then(() => call<T>(method, path, body, retries - 1));
  if (!r.ok) {
    let msg = r.statusText;
    try {
      const j = await r.json();
      msg = j.error?.message || j.error || j.message || msg;
    } catch {
      /* not JSON */
    }
    throw new ApiError(r.status, String(msg));
  }
  if (r.status === 204) return undefined as T;
  const ct = r.headers.get("content-type") || "";
  return (ct.includes("json") ? r.json() : undefined) as Promise<T>;
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const enc = (s: string) => encodeURIComponent(s);
export const filePath = (p: string) => p.split("/").map(enc).join("/");

export const api = {
  me: () => call<Me>("GET", "/api/me"),
  conversations: () => call<ConversationSummary[]>("GET", "/api/conversations"),
  create: (model?: string) => call<{ id: string }>("POST", "/api/conversations", model ? { model } : {}),
  conversation: (id: string) => call<Conversation>("GET", `/api/conversations/${enc(id)}`),
  update: (id: string, patch: { title?: string; model?: string }) => call<void>("PATCH", `/api/conversations/${enc(id)}`, patch),
  remove: (id: string) => call<void>("DELETE", `/api/conversations/${enc(id)}`),
  /** typing: keeps the open conversation's sandbox, or lets the server get a host ready (no sandbox is made) */
  typing: (conversationId: string | null) => call<void>("POST", "/api/typing", conversationId ? { conversationId } : {}, 0),
  send: (id: string, text: string, attachments: string[]) =>
    call<{ turnId: string }>("POST", `/api/conversations/${enc(id)}/messages`, { text, attachments }),
  cancel: (turnId: string) => call<void>("POST", `/api/turns/${enc(turnId)}/cancel`),
  /** a file's bytes (`id`: a conversation's, or shareRef(<share id>) on a share page); `v` = a version (sha256), `inline` = shown in the page (images), `preview` = a small image (PDF page) */
  fileUrl: (id: string, path: string, o: { v?: string; inline?: boolean; preview?: boolean; page?: number } = {}) => {
    const q = new URLSearchParams();
    if (o.v) q.set("v", o.v);
    if (o.inline) q.set("inline", "1");
    if (o.preview) q.set("preview", "1");
    if (o.page && o.page > 1) q.set("page", String(o.page));
    const qs = q.toString();
    // a share page's files (id = shareRef(<share id>)) come from the share's own download route
    const base = id.startsWith("share:") ? `/api/shares/${enc(id.slice(6))}` : `/api/conversations/${enc(id)}`;
    return `${base}/files/${filePath(path)}${qs ? `?${qs}` : ""}`;
  },
  createShare: (id: string) => call<ShareLink>("POST", `/api/conversations/${enc(id)}/shares`),
  shares: (id: string) => call<ShareLink[]>("GET", `/api/conversations/${enc(id)}/shares`),
  revokeShare: (shareId: string) => call<void>("DELETE", `/api/shares/${enc(shareId)}`),
  share: (shareId: string) => call<SharedConversation>("GET", `/api/shares/${enc(shareId)}`),
  continueShare: (shareId: string) => call<{ id: string }>("POST", `/api/shares/${enc(shareId)}/continue`),
  streamUrl: (id: string, after: number) => `/api/conversations/${enc(id)}/stream?after=${after}`,
  /** Raw body upload with progress (XHR: fetch has no upload progress). */
  upload(id: string, file: File, onProgress: (f: number) => void): Promise<{ path: string }> {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest();
      x.open("POST", `/api/conversations/${enc(id)}/files?name=${enc(file.name)}`);
      x.setRequestHeader("content-type", file.type || "application/octet-stream");
      x.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
      x.onload = () => {
        if (x.status >= 200 && x.status < 300) {
          try {
            resolve(JSON.parse(x.responseText));
          } catch {
            reject(new ApiError(x.status, "bad response"));
          }
        } else reject(new ApiError(x.status, x.statusText));
      };
      x.onerror = () => reject(new ApiError(0, "network"));
      x.send(file);
    });
  },
};
