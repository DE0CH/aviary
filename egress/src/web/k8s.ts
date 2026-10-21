// The few Kubernetes API calls the sandbox manager makes (pods in one namespace), as the pod's service account.
// In the cluster: KUBERNETES_SERVICE_HOST/PORT, the token and CA under /var/run/secrets/kubernetes.io/serviceaccount.
// Tests point it at a fake API over plain HTTP.
import { readFileSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

export interface Pod {
  metadata: { name: string; labels?: Record<string, string>; creationTimestamp?: string; deletionTimestamp?: string };
  spec?: unknown;
  status?: { phase?: string; podIP?: string; reason?: string; message?: string; containerStatuses?: { restartCount?: number; state?: Record<string, { reason?: string }> }[] };
}

export interface K8sApi {
  createPod(ns: string, pod: object): Promise<Pod>;
  getPod(ns: string, name: string): Promise<Pod | null>;
  deletePod(ns: string, name: string): Promise<void>;
  listPods(ns: string, labelSelector: string): Promise<Pod[]>;
}

const SA = "/var/run/secrets/kubernetes.io/serviceaccount";

export class K8sClient implements K8sApi {
  readonly #base: URL;
  readonly #token: () => string;
  readonly #ca: Buffer | undefined;
  constructor(o: { base: string; token: () => string; ca?: Buffer }) {
    this.#base = new URL(o.base);
    this.#token = o.token;
    this.#ca = o.ca;
  }
  static inCluster(): K8sClient {
    const host = process.env.KUBERNETES_SERVICE_HOST;
    const port = process.env.KUBERNETES_SERVICE_PORT || "443";
    if (!host) throw new Error("not in a cluster (KUBERNETES_SERVICE_HOST unset)");
    // the projected token rotates: read it on every call
    return new K8sClient({ base: `https://${host.includes(":") ? `[${host}]` : host}:${port}`, token: () => readFileSync(`${SA}/token`, "utf8").trim(), ca: readFileSync(`${SA}/ca.crt`) });
  }
  #req(method: string, path: string, body?: object): Promise<{ status: number; json: any }> {
    const url = new URL(path, this.#base);
    const data = body ? Buffer.from(JSON.stringify(body)) : undefined;
    return new Promise((resolve, reject) => {
      const req = (url.protocol === "https:" ? httpsRequest : httpRequest)(
        url,
        {
          method,
          ca: this.#ca,
          headers: { authorization: `Bearer ${this.#token()}`, accept: "application/json", ...(data ? { "content-type": "application/json", "content-length": String(data.length) } : {}) },
          timeout: 30_000,
        },
        (res) => {
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.push(c));
          res.on("end", () => {
            const text = Buffer.concat(chunks).toString("utf8");
            let json: any = null;
            try {
              json = text ? JSON.parse(text) : null;
            } catch {
              json = { message: text.slice(0, 300) };
            }
            resolve({ status: res.statusCode ?? 0, json });
          });
        },
      );
      req.on("timeout", () => req.destroy(new Error(`k8s ${method} ${path}: timeout`)));
      req.on("error", reject);
      if (data) req.write(data);
      req.end();
    });
  }
  async createPod(ns: string, pod: object): Promise<Pod> {
    const r = await this.#req("POST", `/api/v1/namespaces/${ns}/pods`, pod);
    if (r.status >= 300) throw new Error(`k8s create pod: ${r.status} ${r.json?.message ?? ""}`);
    return r.json as Pod;
  }
  async getPod(ns: string, name: string): Promise<Pod | null> {
    const r = await this.#req("GET", `/api/v1/namespaces/${ns}/pods/${encodeURIComponent(name)}`);
    if (r.status === 404) return null;
    if (r.status >= 300) throw new Error(`k8s get pod: ${r.status} ${r.json?.message ?? ""}`);
    return r.json as Pod;
  }
  async deletePod(ns: string, name: string): Promise<void> {
    const r = await this.#req("DELETE", `/api/v1/namespaces/${ns}/pods/${encodeURIComponent(name)}?gracePeriodSeconds=5`);
    if (r.status >= 300 && r.status !== 404) throw new Error(`k8s delete pod: ${r.status} ${r.json?.message ?? ""}`);
  }
  async listPods(ns: string, labelSelector: string): Promise<Pod[]> {
    const r = await this.#req("GET", `/api/v1/namespaces/${ns}/pods?labelSelector=${encodeURIComponent(labelSelector)}`);
    if (r.status >= 300) throw new Error(`k8s list pods: ${r.status} ${r.json?.message ?? ""}`);
    return (r.json?.items ?? []) as Pod[];
  }
}
