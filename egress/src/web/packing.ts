// Packing (docs/SANDBOX-VM.md, "Packing"): where the vm scaler would like every sandbox to be, as a pure function.
// Hosts within their minimum lifetime are already paid for, so using them is free; the plan minimises the hosts that
// are past it and still used. Releasing a host is not this code's business: a host left empty by the moves goes by
// the release rule (no sandbox and past its minimum lifetime) on its own.
//
//   target = current placement
//   for each host h past its minimum lifetime, least loaded first (ties: the one whose sandboxes moved least recently):
//     try to place h's sandboxes (biggest first) on the other hosts still in use or paid for, paid hosts first, then
//       the fullest one that fits (best fit)
//     all fit -> h leaves the target (its sandboxes take those places); else h stays
//   moves = sandboxes whose target host differs, only if the target uses fewer hosts past their minimum lifetime

export interface PackHost {
  name: string;
  /** slots it has (memory and CPU by the configured sandbox size, the operator's cap) */
  slots: number;
  /** still within its minimum lifetime: already paid for */
  paid: boolean;
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
  const inUse = (h: PackHost) => sandboxes.some((s) => target.get(s.id) === h.name);
  const costly = (where: Map<string, string>) => new Set(sandboxes.map((s) => where.get(s.id)!)).size - hosts.filter((h) => h.paid && sandboxes.some((s) => where.get(s.id) === h.name)).length;

  const lastMove = (h: string) => Math.max(0, ...sandboxes.filter((s) => s.host === h).map((s) => s.movedAt ?? 0));
  const candidates = hosts.filter((h) => !h.paid && inUse(h)).sort((a, b) => load(a.name) - load(b.name) || lastMove(a.name) - lastMove(b.name));
  for (const h of candidates) {
    const leaving = sandboxes.filter((s) => target.get(s.id) === h.name).sort((a, b) => b.size - a.size);
    const trial = new Map(target);
    const free = new Map(hosts.filter((x) => x.open && x.name !== h.name && (x.paid || inUse(x))).map((x) => [x.name, x.slots - load(x.name)]));
    const paid = new Set(hosts.filter((x) => x.paid).map((x) => x.name));
    const fits = leaving.every((s) => {
      const options = [...free].filter(([, f]) => f >= s.size).sort(([a, fa], [b, fb]) => Number(paid.has(b)) - Number(paid.has(a)) || fa - fb);
      const to = options[0]?.[0];
      if (!to) return false;
      free.set(to, free.get(to)! - s.size);
      trial.set(s.id, to);
      return true;
    });
    if (fits) for (const [id, to] of trial) target.set(id, to);
  }

  const current = new Map(sandboxes.map((s) => [s.id, s.host]));
  if (costly(target) >= costly(current)) return [];
  return sandboxes.filter((s) => target.get(s.id) !== s.host).map((s) => ({ id: s.id, from: s.host, to: target.get(s.id)! }));
}
