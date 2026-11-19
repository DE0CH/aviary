// Test helpers for egress's own database: a fresh, migrated `egress` database per test file on the local Postgres
// (TEST_DATABASE_URL), reached through a TCP relay the test can cut to play a database outage.
import pg from "pg";
import { createServer, connect as netConnect, type AddressInfo, type Server, type Socket } from "node:net";
import { connect, migrate, type Db } from "../src/egress/db.ts";

/** A TCP relay in front of Postgres: `cut()` drops every connection and refuses new ones until `restore()`;
 * `stall()` plays a database that hangs instead (a lost node, blackholed packets): open connections stop carrying
 * bytes and new ones are accepted but never answered, until `restore()` (which then drops the stalled ones). */
export class PgRelay {
  readonly #server: Server;
  readonly #socks = new Set<Socket>();
  #down = false;
  #stalled = false;
  port = 0;
  readonly host: string;
  readonly target: number;
  constructor(host: string, target: number) {
    this.host = host;
    this.target = target;
    this.#server = createServer((c) => {
      if (this.#down) return void c.destroy();
      if (this.#stalled) {
        this.#socks.add(c);
        c.on("error", () => {}).on("close", () => this.#socks.delete(c));
        c.pause();
        return;
      }
      const u = netConnect(this.target, this.host);
      this.#socks.add(c).add(u);
      const end = () => {
        c.destroy();
        u.destroy();
        this.#socks.delete(c);
        this.#socks.delete(u);
      };
      c.on("error", end).on("close", end);
      u.on("error", end).on("close", end);
      c.pipe(u).pipe(c);
    });
  }
  async listen(): Promise<number> {
    await new Promise<void>((r) => this.#server.listen(0, "127.0.0.1", r));
    this.port = (this.#server.address() as AddressInfo).port;
    return this.port;
  }
  cut(): void {
    this.#down = true;
    for (const s of this.#socks) s.destroy();
  }
  stall(): void {
    this.#stalled = true;
    for (const s of this.#socks) {
      s.unpipe();
      s.pause();
    }
  }
  restore(): void {
    if (this.#stalled) for (const s of this.#socks) s.destroy();
    this.#down = false;
    this.#stalled = false;
  }
  close(): void {
    this.cut();
    this.#server.close();
  }
}

export async function freshEgressDb(name: string, viaRelay = false): Promise<{ db: Db; url: string; relay: PgRelay | null; drop(): Promise<void> }> {
  const admin = process.env.TEST_DATABASE_URL || "postgres://claude:dev@127.0.0.1:5432/postgres";
  const dbName = `egress_test_${name}_${process.pid}`.toLowerCase().replace(/[^a-z0-9_]/g, "_");
  const c = new pg.Client({ connectionString: admin });
  await c.connect();
  await c.query(`drop database if exists ${dbName} with (force)`);
  await c.query(`create database ${dbName}`);
  await c.end();
  const u = new URL(admin);
  u.pathname = `/${dbName}`;
  let relay: PgRelay | null = null;
  if (viaRelay) {
    relay = new PgRelay(u.hostname, Number(u.port || 5432));
    u.port = String(await relay.listen());
    u.hostname = "127.0.0.1";
  }
  const url = u.toString();
  const db = connect(url);
  await migrate(db);
  return {
    db,
    url,
    relay,
    async drop() {
      await db.end().catch(() => {});
      relay?.close();
      const c2 = new pg.Client({ connectionString: admin });
      await c2.connect();
      await c2.query(`drop database if exists ${dbName} with (force)`);
      await c2.end();
    },
  };
}
