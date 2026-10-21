// Packing (docs/SANDBOX-VM.md, "Packing"): where the vm scaler would like every sandbox to be, as a pure function:
// all sandboxes on the fewest hosts. Host age plays no part here; a host the moves leave empty goes by the release
// rule (no sandbox and past its minimum lifetime) on its own.
//
//   target = current placement
//   for each host h in use, least loaded first (ties: the one whose sandboxes moved least recently):
//     place h's sandboxes, biggest first, on the other hosts still in use, each on the fullest one it fits (best fit)
//     all fit -> they go there in the target (h is empty in it); else h keeps them
//   moves = sandboxes whose target host differs, only if the target uses strictly fewer hosts

export interface PackHost {
  name: string;
  /** slots it has (memory and CPU by the configured sandbox size, the operator's cap) */
  slots: number;
  /** may take sandboxes (active and answering, not short of memory) */
  open: boolean;
}

export interface PackSandbox {
  id: string;
  host: string;
  /** slots it takes (a sandbox from before a smaller sandbox.memory takes more) */
  size: number;
  /** when it was last moved (ms since epoch), 0 = never: the tie-breaker between equally good plans */
  movedAt?: number;
}

export interface Move {
  id: string;
  from: string;
  to: string;
}

export function planPacking(hosts: PackHost[], sandboxes: PackSandbox[]): Move[] {
  const target = new Map(sandboxes.map((s) => [s.id, s.host]));
  const load = (h: string) => sandboxes.reduce((a, s) => a + (target.get(s.id) === h ? s.size : 0), 0);
  const inUse = (h: string) => sandboxes.some((s) => target.get(s.id) === h);
  const used = (where: Map<string, string>) => new Set(where.values()).size;
  const lastMove = (h: string) => Math.max(0, ...sandboxes.filter((s) => s.host === h).map((s) => s.movedAt ?? 0));

  const candidates = hosts.filter((h) => inUse(h.name)).sort((a, b) => load(a.name) - load(b.name) || lastMove(a.name) - lastMove(b.name));
  for (const h of candidates) {
    const leaving = sandboxes.filter((s) => target.get(s.id) === h.name).sort((a, b) => b.size - a.size);
    const free = new Map(hosts.filter((x) => x.open && x.name !== h.name && inUse(x.name)).map((x) => [x.name, x.slots - load(x.name)]));
    const trial = new Map(target);
    const fits = leaving.every((s) => {
      const to = [...free].filter(([, f]) => f >= s.size).sort(([, a], [, b]) => a - b)[0]?.[0];
      if (!to) return false;
      free.set(to, free.get(to)! - s.size);
      trial.set(s.id, to);
      return true;
    });
    if (fits) for (const [id, to] of trial) target.set(id, to);
  }

  if (used(target) >= used(new Map(sandboxes.map((s) => [s.id, s.host])))) return [];
  return sandboxes.filter((s) => target.get(s.id) !== s.host).map((s) => ({ id: s.id, from: s.host, to: target.get(s.id)! }));
}
