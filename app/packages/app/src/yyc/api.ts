/**
 * CloudCLI's own REST API, for the YYC features Paseo's protocol has no message for (sharing, uploads, warming).
 * The page is served by the CloudCLI server it talks to, so requests are same-origin and carry the Cloudflare
 * Access cookie. In development the app runs on Metro's port and EXPO_PUBLIC_LOCAL_DAEMON names the server.
 */
function apiBase(): string {
  const dev = process.env.EXPO_PUBLIC_LOCAL_DAEMON?.trim();
  if (dev) return `http://${dev}`;
  return typeof window === "undefined" ? "" : window.location.origin;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string | null,
  ) {
    super(message);
  }
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`${apiBase()}${path}`, {
    credentials: "include",
    ...init,
    headers: { "content-type": "application/json", ...(init.headers ?? {}) },
  });
  const text = await response.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!response.ok) {
    const error = (body as { error?: { message?: string; code?: string } } | null)?.error;
    throw new ApiError(error?.message ?? `HTTP ${response.status}`, response.status, error?.code ?? null);
  }
  return body as T;
}

/** The CloudCLI session id behind a Paseo workspace id (the bridge names workspaces `wks_<session id>`). */
export function sessionIdOfWorkspace(workspaceId: string | null): string | null {
  return workspaceId?.startsWith("wks_") ? workspaceId.slice(4) : null;
}

export function absoluteUrl(path: string): string {
  return typeof window === "undefined" ? path : new URL(path, window.location.origin).toString();
}
