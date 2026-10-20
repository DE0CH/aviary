// An in-memory S3 (path-style PUT/GET/HEAD/DELETE, signatures ignored) for the web role's tests.
import { createServer, type Server } from "node:http";

export async function fakeS3(): Promise<{ url: string; objects: Map<string, Buffer>; close(): Promise<void> }> {
  const objects = new Map<string, Buffer>();
  const server: Server = createServer(async (req, res) => {
    const key = decodeURIComponent(new URL(req.url || "/", "http://x").pathname.replace(/^\/[^/]+\//, ""));
    if (req.method === "PUT") {
      const chunks: Buffer[] = [];
      for await (const c of req) chunks.push(c as Buffer);
      objects.set(key, Buffer.concat(chunks));
      res.writeHead(200).end();
    } else if (req.method === "GET" || req.method === "HEAD") {
      const o = objects.get(key);
      if (!o) return void res.writeHead(404).end("NoSuchKey");
      res.writeHead(200, { "content-length": o.length });
      res.end(req.method === "GET" ? o : undefined);
    } else if (req.method === "DELETE") {
      objects.delete(key);
      res.writeHead(204).end();
    } else res.writeHead(405).end();
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as { port: number }).port;
  return { url: `http://127.0.0.1:${port}`, objects, close: () => new Promise((r) => server.close(() => r())) };
}
