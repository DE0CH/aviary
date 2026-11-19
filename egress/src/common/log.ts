// One JSON line per log record on stdout (the cluster's log collector reads it). Never log a secret or a token.
export function log(at: string, fields: Record<string, unknown> = {}): void {
  process.stdout.write(JSON.stringify({ t: new Date().toISOString(), at, ...fields }) + "\n");
}

export function logError(at: string, e: unknown, fields: Record<string, unknown> = {}): void {
  const t = new Date().toISOString();
  const error = e instanceof Error ? e.message : String(e);
  process.stderr.write(JSON.stringify({ t, at, error, ...fields }) + "\n");
}

/**
 * Removes what could be a credential from a log line: bearer/basic values, user:password@ in URLs, token=/key=
 * parameters, and long opaque strings (base64url/hex runs of 24+ characters).
 */
export function redact(s: string): string {
  return s
    .replace(/\b(bearer|basic)\s+[^\s"',;]+/gi, "$1 [redacted]")
    .replace(/(\/\/)[^\s/@:"']+:[^\s/@"']+@/g, "$1[redacted]@")
    .replace(/\b([a-z_-]*(?:token|key|secret|password|passwd|authorization|signature|sig)[a-z_-]*)(["']?\s*[=:]\s*["']?)[^\s"'&,;}]+/gi, "$1$2[redacted]")
    .replace(/[A-Za-z0-9_\-+/]{24,}={0,2}/g, (m) => (UUID.test(m) || !/\d/.test(m) || !/[a-z]/i.test(m) ? m : "[redacted]"));
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
