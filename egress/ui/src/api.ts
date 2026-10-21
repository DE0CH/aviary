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

export interface Turn {
  id: string;
  status: "queued" | "running" | "waiting" | "done" | "error" | "cancelled";
  startedAt?: string;
  finishedAt?: string | null;
  error?: string | null;
  events: ChatEvent[];
}

export interface Conversation {
  id: string;
  title: string;
  model: string;
  messages: Message[];
  turns: Turn[];
}

export interface FileEntry {
  path: string;
  size: number;
  source: "upload" | "agent";
  updatedAt: string;
}

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
  files: (id: string) => call<FileEntry[]>("GET", `/api/conversations/${enc(id)}/files`),
  fileUrl: (id: string, path: string) => `/api/conversations/${enc(id)}/files/${filePath(path)}`,
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
