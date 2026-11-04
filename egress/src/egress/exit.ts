// The deployment's way out (egress.json `exit`): egress hands every internet connection to a SOCKS5 server by host
// name, and that server resolves the name, checks the address and connects (YYC: its yyc-socks pool, DE0CH/YYC
// socks/README.md). With an exit, egress neither resolves internet names nor checks addresses itself: the exit's
// `sandbox` profile does both (malware-blocking DNS, public addresses only) and answers why it refused, which egress
// logs as the same decisions it logs when it resolves on its own (deny:threat, deny:nxdomain, deny:dns, deny:address).
// The credential routes' outside upstreams (the model route: OpenRouter) go through the exit too, on its default
// profile; names inside the cluster and private addresses are reached directly.
import type { RequestOptions } from "node:http";
import { Agent as HttpsAgent, type AgentOptions } from "node:https";
import type { Duplex } from "node:stream";
import { connect as netConnect, isIP, type Socket } from "node:net";
import { connect as tlsConnect, type ConnectionOptions } from "node:tls";

export interface ExitConfig {
  /** host:port of the SOCKS5 server */
  host: string;
  port: number;
  /** the profile sandbox connections ask for (SOCKS5 username; default "sandbox") */
  sandboxProfile: string;
}

export function parseExit(v: unknown): ExitConfig | undefined {
  if (v === undefined || v === null) return undefined;
  if (typeof v !== "object") throw new Error("egress config: exit must be an object");
  const o = v as Record<string, unknown>;
  if (typeof o.socks !== "string" || !o.socks) throw new Error('egress config: exit.socks must be "host:port"');
  const m = /^\[?([^\]]+?)\]?:(\d+)$/.exec(o.socks);
  if (!m || !(Number(m[2]) > 0 && Number(m[2]) < 65536)) throw new Error(`egress config: exit.socks ${JSON.stringify(o.socks)} is not host:port`);
  if (o.sandboxProfile !== undefined && (typeof o.sandboxProfile !== "string" || !o.sandboxProfile)) throw new Error("egress config: exit.sandboxProfile must be a non-empty string");
  return { host: m[1]!, port: Number(m[2]), sandboxProfile: (o.sandboxProfile as string | undefined) ?? "sandbox" };
}

/** Why the exit didn't connect: a refusal egress logs as a deny decision, or a failure to reach the site. */
export class ExitRefusal extends Error {
  readonly reason: "deny:threat" | "deny:nxdomain" | "deny:dns" | "deny:address" | "connect";
  constructor(reason: ExitRefusal["reason"], message: string) {
    super(message);
    this.reason = reason;
  }
}

/** SOCKS5 reply codes (RFC 1928 section 6) and yyc-socks' 0x80 (blocked as a threat), as egress's decisions. */
function refusalFor(code: number, host: string, port: number): ExitRefusal {
  switch (code) {
    case 0x80:
      return new ExitRefusal("deny:threat", `${host} is blocked as a security threat (malware or phishing)`);
    case 0x02:
      return new ExitRefusal("deny:address", `${host} resolves only to non-public addresses`);
    case 0x04:
      return new ExitRefusal("deny:nxdomain", `${host} does not exist (or is blocked as a security threat)`);
    case 0x01:
      return new ExitRefusal("deny:dns", `could not resolve ${host}`);
    case 0x05:
      return new ExitRefusal("connect", `${host}:${port} refused the connection`);
    case 0x06:
      return new ExitRefusal("connect", `${host}:${port} did not answer in time`);
    default:
      return new ExitRefusal("connect", `could not connect to ${host}:${port} (exit reply ${code})`);
  }
}

/**
 * A TCP connection to host:port through the exit, by name (an IP literal is sent as an address). `profile`: the SOCKS5
 * username naming the exit's profile (absent: no authentication, the default profile). Rejects with ExitRefusal.
 */
export function exitConnect(exit: ExitConfig, host: string, port: number, profile?: string, timeoutMs = 20_000): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const s = netConnect({ host: exit.host, port: exit.port });
    let buf = Buffer.alloc(0);
    let step: "method" | "auth" | "reply" = "method";
    let done = false;
    const fail = (e: Error) => {
      if (done) return;
      done = true;
      s.destroy();
      reject(e);
    };
    const timer = setTimeout(() => fail(new ExitRefusal("connect", `the exit did not connect to ${host}:${port} in time`)), timeoutMs);
    const finish = () => {
      clearTimeout(timer);
      s.removeAllListeners("data");
      s.removeListener("error", onError);
      done = true;
      resolve(s);
    };
    const onError = (e: Error) => {
      clearTimeout(timer);
      fail(new ExitRefusal("connect", `the exit could not be reached: ${e.message}`));
    };
    const sendRequest = () => {
      const h = host.replace(/^\[|\]$/g, "");
      const kind = isIP(h);
      let addr: Buffer;
      if (kind === 4) addr = Buffer.from([1, ...h.split(".").map(Number)]);
      else if (kind === 6) addr = Buffer.concat([Buffer.from([4]), ipv6Bytes(h)]);
      else {
        const name = Buffer.from(h, "utf8");
        if (name.length > 255) return fail(new ExitRefusal("deny:nxdomain", `${host} is not a host name`));
        addr = Buffer.concat([Buffer.from([3, name.length]), name]);
      }
      const p = Buffer.alloc(2);
      p.writeUInt16BE(port);
      s.write(Buffer.concat([Buffer.from([5, 1, 0]), addr, p]));
      step = "reply";
    };
    s.once("connect", () => {
      s.write(profile ? Buffer.from([5, 1, 2]) : Buffer.from([5, 1, 0]));
    });
    s.on("error", onError);
    s.on("close", () => fail(new ExitRefusal("connect", `the exit closed the connection to ${host}:${port}`)));
    s.on("data", (c: Buffer) => {
      buf = Buffer.concat([buf, c]);
      if (step === "method") {
        if (buf.length < 2) return;
        const m = buf[1]!;
        buf = buf.subarray(2);
        if (profile && m === 2) {
          const u = Buffer.from(profile, "utf8");
          s.write(Buffer.concat([Buffer.from([1, u.length]), u, Buffer.from([1]), Buffer.from("x")]));
          step = "auth";
        } else if (!profile && m === 0) sendRequest();
        else return fail(new ExitRefusal("connect", `the exit refused the method (${m})`));
      }
      if (step === "auth") {
        if (buf.length < 2) return;
        if (buf[1] !== 0) return fail(new ExitRefusal("connect", `the exit refused the profile ${JSON.stringify(profile)}`));
        buf = buf.subarray(2);
        sendRequest();
      }
      if (step === "reply") {
        if (buf.length < 5) return;
        const code = buf[1]!;
        if (code !== 0) {
          clearTimeout(timer);
          return fail(refusalFor(code, host.replace(/^\[|\]$/g, ""), port));
        }
        const atyp = buf[3]!;
        const len = atyp === 1 ? 4 : atyp === 4 ? 16 : atyp === 3 ? 1 + buf[4]! : -1;
        if (len < 0) return fail(new ExitRefusal("connect", `bad exit reply (address type ${atyp})`));
        const total = 4 + len + 2;
        if (buf.length < total) return;
        const rest = buf.subarray(total);
        finish();
        if (rest.length) s.unshift(rest);
      }
    });
  });
}

function ipv6Bytes(a: string): Buffer {
  const [head, tail] = a.includes("::") ? a.split("::") : [a, undefined];
  const h = head ? head.split(":") : [];
  const t = tail !== undefined && tail !== "" ? tail.split(":") : [];
  // an embedded IPv4 tail (::ffff:1.2.3.4) is two groups
  const groups = (xs: string[]) => xs.flatMap((g) => (g.includes(".") ? (() => { const p = g.split(".").map(Number); return [((p[0]! << 8) | p[1]!).toString(16), ((p[2]! << 8) | p[3]!).toString(16)]; })() : [g]));
  const hh = groups(h), tt = groups(t);
  const all = tail !== undefined ? [...hh, ...Array(8 - hh.length - tt.length).fill("0"), ...tt] : hh;
  const b = Buffer.alloc(16);
  all.forEach((g, i) => b.writeUInt16BE(parseInt(g || "0", 16), i * 2));
  return b;
}

/** True for a host egress reaches directly even with an exit: a name inside the cluster or a private address. */
export function internalHost(host: string): boolean {
  const h = host.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "");
  if (h === "localhost" || h.endsWith(".cluster.local") || h.endsWith(".svc") || !h.includes(".") && !isIP(h)) return true;
  if (isIP(h) === 4) {
    const [a, b] = h.split(".").map(Number) as [number, number];
    return a === 10 || a === 127 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254) || (a === 100 && b >= 64 && b <= 127);
  }
  if (isIP(h) === 6) return h === "::1" || /^f[cd]/.test(h) || /^fe[89ab]/.test(h);
  return false;
}

/** An https.Agent whose connections go through the exit (default profile), TLS on top as usual. */
export class ExitHttpsAgent extends HttpsAgent {
  readonly #exit: ExitConfig;
  constructor(exit: ExitConfig, o: AgentOptions = {}) {
    super(o);
    this.#exit = exit;
  }
  override createConnection(o: RequestOptions, cb?: (err: Error | null, stream: Duplex) => void): undefined {
    const tlsOpts = o as RequestOptions & ConnectionOptions;
    const host = String(o.host ?? o.hostname ?? "localhost");
    const port = Number(o.port ?? 443);
    exitConnect(this.#exit, host, port).then(
      (raw) => {
        const servername = tlsOpts.servername || (isIP(host) ? undefined : host);
        const t = tlsConnect({ ...tlsOpts, socket: raw, ...(servername ? { servername } : {}) } as ConnectionOptions);
        cb?.(null, t);
      },
      (e: Error) => cb?.(e, undefined as unknown as Duplex),
    );
    return undefined;
  }
}
