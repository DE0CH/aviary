// One JSON line per log record on stdout (the cluster's log collector reads it). Never log a secret or a token.
export function log(at: string, fields: Record<string, unknown> = {}): void {
  process.stdout.write(JSON.stringify({ t: new Date().toISOString(), at, ...fields }) + "\n");
}

export function logError(at: string, e: unknown, fields: Record<string, unknown> = {}): void {
  process.stderr.write(JSON.stringify({ t: new Date().toISOString(), at, error: e instanceof Error ? e.message : String(e), ...fields }) + "\n");
}
