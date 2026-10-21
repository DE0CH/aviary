// Clients the "vm" sandbox provider uses (docs/SANDBOX-VM.md): sandboxd on each host (HTTPS with mutual TLS over the
// overlay), EC2 for the AWS burst hosts (the Query API, signed with aws4fetch), and WireGuard key pairs for new hosts.
import { generateKeyPairSync } from "node:crypto";
import { Agent, request } from "node:https";
import type { PeerCertificate } from "node:tls";
import { AwsClient } from "aws4fetch";

// ---- sandboxd ------------------------------------------------------------------------------------------------

export interface HostSandbox {
  name: string;
  ip: string;
  state: "running" | "receiving";
  createdAt: string;
  cpu: number;
  memory: number;
  cpuUsage: number;
  memoryCurrent: number;
}

export interface HostInfo {
  name: string;
  role: "hub" | "burst";
  /** the hub as a gateway only (on a k3s node): no sandboxes, its CPU features don't matter */
  gateway?: boolean;
  wgAddress: string;
  wgPublicKey: string;
  peers: { publicKey: string; endpoint: string; allowedIps: string[]; latestHandshake: number }[];
  egressTargets: string[];
  memTotal: number;
  memAvailable: number;
  reserveMemory: number;
  cpus: number;
  loadAvg?: number[];
  uptimeS?: number;
  cpuFeatures: string;
  runsc: string;
  gvisorRelease: string;
  images: string[];
  bootId: string;
  startedAt: string;
  sandboxes: HostSandbox[];
}

export interface SandboxdApi {
  host(url: string, timeoutMs?: number): Promise<HostInfo>;
  call<T = any>(url: string, method: string, path: string, body?: unknown, timeoutMs?: number): Promise<T>;
}

export class SandboxdError extends Error {
  readonly status: number;
  constructor(status: number, msg: string) {
    super(msg);
    this.status = status;
  }
}

/** sandboxd over mutual TLS: our CA signs the hosts' certificates (name "sandboxd") and the controller's. */
export class SandboxdClient implements SandboxdApi {
  readonly #agent: Agent;
  constructor(tls: { ca: string; cert: string; key: string }) {
    this.#agent = new Agent({
      ca: tls.ca,
      cert: tls.cert,
      key: tls.key,
      keepAlive: true,
      maxSockets: 16,
      minVersion: "TLSv1.3",
      // hosts are addressed by overlay IP; every host certificate is for the name "sandboxd"
      servername: "sandboxd",
      checkServerIdentity: (_h: string, cert: PeerCertificate) =>
        cert.subject?.CN === "sandboxd" || /DNS:sandboxd(,|$)/.test(cert.subjectaltname ?? "") ? undefined : new Error("not a sandboxd certificate"),
    });
  }

  host(url: string, timeoutMs = 5000): Promise<HostInfo> {
    return this.call<HostInfo>(url, "GET", "/v1/host", undefined, timeoutMs);
  }

  call<T = any>(url: string, method: string, path: string, body?: unknown, timeoutMs = 30_000): Promise<T> {
    const u = new URL(path, url);
    const data = body === undefined ? undefined : JSON.stringify(body);
    return new Promise<T>((resolve, reject) => {
      const req = request(
        u,
        { method, agent: this.#agent, headers: data ? { "content-type": "application/json", "content-length": Buffer.byteLength(data) } : {}, timeout: timeoutMs },
        (res) => {
          let s = "";
          res.setEncoding("utf8");
          res.on("data", (c) => (s += c));
          res.on("end", () => {
            let j: any = null;
            try {
              j = s ? JSON.parse(s) : null;
            } catch {}
            if ((res.statusCode ?? 0) >= 400) reject(new SandboxdError(res.statusCode ?? 0, `sandboxd ${method} ${u.pathname}: ${res.statusCode} ${j?.error ?? s.slice(0, 200)}`));
            else resolve(j as T);
          });
        },
      );
      req.on("timeout", () => req.destroy(new Error(`sandboxd ${method} ${u.pathname}: timed out after ${timeoutMs} ms`)));
      req.on("error", reject);
      req.end(data);
    });
  }
}

// ---- EC2 -----------------------------------------------------------------------------------------------------

export interface Ec2Instance {
  id: string;
  state: string;
  privateIp: string;
  launchTime: string;
  tags: Record<string, string>;
}

export interface Ec2Api {
  runInstance(o: { launchTemplate: string; userData: string; tags: Record<string, string> }): Promise<Ec2Instance>;
  terminate(ids: string[]): Promise<void>;
  describe(tags: Record<string, string>): Promise<Ec2Instance[]>;
}

function xmlAll(xml: string, tag: string): string[] {
  const out: string[] = [];
  const re = new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(xml))) out.push(m[1]!);
  return out;
}
const xml1 = (xml: string, tag: string): string => new RegExp(`<${tag}>([^<]*)</${tag}>`).exec(xml)?.[1] ?? "";

/** Instances from a RunInstances/DescribeInstances answer (pure: tested). */
export function parseInstances(xml: string): Ec2Instance[] {
  // each <item> holding an <instanceId> directly under instancesSet
  return xmlAll(xml, "instancesSet")
    .flatMap((set) => set.split(/<item>\s*<instanceId>/).slice(1).map((chunk) => `<instanceId>${chunk}`))
    .map((x) => {
      const tags: Record<string, string> = {};
      for (const t of xmlAll(x, "tagSet").flatMap((ts) => xmlAll(ts, "item"))) tags[xml1(t, "key")] = xml1(t, "value");
      const stateBlock = xmlAll(x, "instanceState")[0] ?? "";
      return { id: xml1(x, "instanceId"), state: xml1(stateBlock, "name"), privateIp: xml1(x, "privateIpAddress"), launchTime: xml1(x, "launchTime"), tags };
    });
}

export class Ec2Client implements Ec2Api {
  readonly #aws: AwsClient;
  readonly #endpoint: string;
  constructor(o: { accessKeyId: string; secretAccessKey: string; region: string }) {
    this.#aws = new AwsClient({ accessKeyId: o.accessKeyId, secretAccessKey: o.secretAccessKey, service: "ec2", region: o.region });
    this.#endpoint = `https://ec2.${o.region}.amazonaws.com/`;
  }

  async #call(params: Record<string, string>): Promise<string> {
    const body = new URLSearchParams({ Version: "2016-11-15", ...params }).toString();
    const r = await this.#aws.fetch(this.#endpoint, { method: "POST", body, headers: { "content-type": "application/x-www-form-urlencoded; charset=utf-8" } });
    const text = await r.text();
    if (!r.ok) throw new Error(`ec2 ${params.Action}: HTTP ${r.status} ${xml1(text, "Code")}: ${xml1(text, "Message").slice(0, 300)}`);
    return text;
  }

  async runInstance(o: { launchTemplate: string; userData: string; tags: Record<string, string> }): Promise<Ec2Instance> {
    const p: Record<string, string> = {
      Action: "RunInstances",
      MinCount: "1",
      MaxCount: "1",
      "LaunchTemplate.LaunchTemplateName": o.launchTemplate,
      "LaunchTemplate.Version": "$Default",
      UserData: Buffer.from(o.userData).toString("base64"),
    };
    let i = 1;
    for (const rt of ["instance", "volume"]) {
      p[`TagSpecification.${i}.ResourceType`] = rt;
      let j = 1;
      for (const [k, v] of Object.entries(o.tags)) {
        p[`TagSpecification.${i}.Tag.${j}.Key`] = k;
        p[`TagSpecification.${i}.Tag.${j}.Value`] = v;
        j++;
      }
      i++;
    }
    const inst = parseInstances(await this.#call(p))[0];
    if (!inst?.id) throw new Error("ec2 RunInstances: no instance in the answer");
    return inst;
  }

  async terminate(ids: string[]): Promise<void> {
    if (!ids.length) return;
    const p: Record<string, string> = { Action: "TerminateInstances" };
    ids.forEach((id, i) => (p[`InstanceId.${i + 1}`] = id));
    await this.#call(p);
  }

  async describe(tags: Record<string, string>): Promise<Ec2Instance[]> {
    const p: Record<string, string> = { Action: "DescribeInstances" };
    let i = 1;
    for (const [k, v] of Object.entries(tags)) {
      p[`Filter.${i}.Name`] = `tag:${k}`;
      p[`Filter.${i}.Value.1`] = v;
      i++;
    }
    p[`Filter.${i}.Name`] = "instance-state-name";
    ["pending", "running", "stopping", "stopped", "shutting-down"].forEach((s, j) => (p[`Filter.${i}.Value.${j + 1}`] = s));
    return parseInstances(await this.#call(p));
  }
}

// ---- WireGuard -----------------------------------------------------------------------------------------------

/** A WireGuard key pair (Curve25519), base64 as `wg` prints them. */
export function wgKeyPair(): { privateKey: string; publicKey: string } {
  const { publicKey, privateKey } = generateKeyPairSync("x25519");
  const b64 = (u: string | undefined) => Buffer.from(u ?? "", "base64url").toString("base64");
  return { privateKey: b64(privateKey.export({ format: "jwk" }).d), publicKey: b64(publicKey.export({ format: "jwk" }).x) };
}
