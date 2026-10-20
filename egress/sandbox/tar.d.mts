import type { Readable } from "node:stream";
export interface TarEntry {
  name: string;
  size: number;
  mode: number;
  mtime: number;
}
export function entryHeader(name: string, size: number, mode?: number, mtimeSec?: number): Buffer;
export function entryPadding(size: number): Buffer;
export const END: Buffer;
export function packBuffers(files: { name: string; data: Buffer | string; mode?: number; mtime?: number }[]): Buffer;
export function unpack(stream: NodeJS.ReadableStream, onFile: (h: TarEntry, body: Readable) => Promise<void> | void): Promise<void>;
