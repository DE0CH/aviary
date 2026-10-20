// Test helper: a fresh, migrated database per test file on the local Postgres (TEST_DATABASE_URL points at a server
// where the user may create databases; default postgres://claude:dev@127.0.0.1:5432/postgres).
import pg from "pg";
import { connect, migrate, type Db } from "../src/common/db.ts";

export async function freshDb(name: string): Promise<{ db: Db; url: string; drop(): Promise<void> }> {
  const admin = process.env.TEST_DATABASE_URL || "postgres://claude:dev@127.0.0.1:5432/postgres";
  const dbName = `chat_test_${name}_${process.pid}`.toLowerCase().replace(/[^a-z0-9_]/g, "_");
  const c = new pg.Client({ connectionString: admin });
  await c.connect();
  await c.query(`drop database if exists ${dbName} with (force)`);
  await c.query(`create database ${dbName}`);
  await c.end();
  const u = new URL(admin);
  u.pathname = `/${dbName}`;
  const url = u.toString();
  const db = connect(url);
  await migrate(db);
  return {
    db,
    url,
    async drop() {
      await db.end();
      const c2 = new pg.Client({ connectionString: admin });
      await c2.connect();
      await c2.query(`drop database if exists ${dbName} with (force)`);
      await c2.end();
    },
  };
}
