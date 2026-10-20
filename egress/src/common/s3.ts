// S3-compatible object store (Hetzner Object Storage in production), signed with aws4fetch, path-style URLs
// <endpoint>/<bucket>/<key>. Adapted from server/src/s3.ts. Keys are content addresses (sha256), so a PUT of an
// existing key is a no-op in effect.
import { AwsClient } from "aws4fetch";
import { createReadStream } from "node:fs";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";
import { Readable } from "node:stream";

export interface BlobStore {
  put(key: string, body: Uint8Array, contentType?: string): Promise<void>;
  /** Streams a local file of known size (never buffered whole). */
  putFile(key: string, path: string, size: number, contentType?: string): Promise<void>;
  get(key: string): Promise<Buffer | null>;
  getStream(key: string): Promise<Readable | null>;
  exists(key: string): Promise<boolean>;
  delete(key: string): Promise<void>;
}

export interface S3Options {
  endpoint: string;
  region: string;
  bucket: string;
  accessKeyId: string;
  secretAccessKey: string;
}

const encKey = (key: string): string => key.split("/").map(encodeURIComponent).join("/");

export class S3BlobStore implements BlobStore {
  readonly #aws: AwsClient;
  readonly #base: string;
  constructor(o: S3Options) {
    this.#aws = new AwsClient({ accessKeyId: o.accessKeyId, secretAccessKey: o.secretAccessKey, region: o.region, service: "s3" });
    this.#base = `${o.endpoint.replace(/\/$/, "")}/${o.bucket}`;
  }
  #url(key: string): string {
    return `${this.#base}/${encKey(key)}`;
  }
  async #req(method: string, key: string, init: { body?: Uint8Array; headers?: Record<string, string> } = {}): Promise<Response> {
    const r = await this.#aws.fetch(this.#url(key), { method, body: init.body as RequestInit["body"], headers: init.headers });
    if (!r.ok && r.status !== 404) throw new Error(`S3 ${method} ${key} -> ${r.status}: ${(await r.text()).slice(0, 200)}`);
    return r;
  }
  async put(key: string, body: Uint8Array, contentType = "application/octet-stream"): Promise<void> {
    const r = await this.#req("PUT", key, { body, headers: { "content-type": contentType } });
    if (!r.ok) throw new Error(`S3 PUT ${key} -> ${r.status}`);
  }
  async putFile(key: string, path: string, size: number, contentType = "application/octet-stream"): Promise<void> {
    const signed = await this.#aws.sign(`${this.#url(key)}?X-Amz-Expires=3600`, { method: "PUT", aws: { signQuery: true, allHeaders: false } });
    const url = new URL(signed.url);
    await new Promise<void>((resolve, reject) => {
      const req = (url.protocol === "https:" ? httpsRequest : httpRequest)(
        url,
        { method: "PUT", headers: { "content-length": String(size), "content-type": contentType } },
        (res) => {
          const chunks: Buffer[] = [];
          res.on("data", (c: Buffer) => chunks.length < 8 && chunks.push(c));
          res.on("end", () =>
            (res.statusCode ?? 0) >= 200 && (res.statusCode ?? 0) < 300
              ? resolve()
              : reject(new Error(`S3 PUT ${key} -> ${res.statusCode}: ${Buffer.concat(chunks).toString().slice(0, 200)}`)),
          );
          res.on("error", reject);
        },
      );
      req.on("error", reject);
      const body = createReadStream(path);
      body.on("error", (e) => {
        req.destroy(e);
        reject(e);
      });
      body.pipe(req);
    });
  }
  async get(key: string): Promise<Buffer | null> {
    const r = await this.#req("GET", key);
    return r.status === 404 ? null : Buffer.from(await r.arrayBuffer());
  }
  async getStream(key: string): Promise<Readable | null> {
    const r = await this.#req("GET", key);
    if (r.status === 404 || !r.body) return null;
    return Readable.fromWeb(r.body as import("node:stream/web").ReadableStream);
  }
  async exists(key: string): Promise<boolean> {
    return (await this.#req("HEAD", key)).ok;
  }
  async delete(key: string): Promise<void> {
    await this.#req("DELETE", key);
  }
}
