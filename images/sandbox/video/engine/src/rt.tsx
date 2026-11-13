// The small runtime the composition is written against: the current frame, the video's size, timed sequences,
// interpolation and seeded randomness. HyperFrames seeks the page; `entry.tsx` turns each seek into one render.
import React, { createContext, useContext } from "react";

export type Config = { fps: number; width: number; height: number; durationInFrames: number };
const FrameCtx = createContext(0);
const ConfigCtx = createContext<Config>({ fps: 30, width: 1080, height: 1920, durationInFrames: 1 });
export const Stage: React.FC<{ frame: number; config: Config; children: React.ReactNode }> = ({ frame, config, children }) => (
  <ConfigCtx.Provider value={config}><FrameCtx.Provider value={frame}>{children}</FrameCtx.Provider></ConfigCtx.Provider>
);
export const useCurrentFrame = () => useContext(FrameCtx);
export const useVideoConfig = () => useContext(ConfigCtx);

export const AbsoluteFill: React.FC<{ style?: React.CSSProperties; children?: React.ReactNode }> = ({ style, children }) => (
  <div style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0, width: "100%", height: "100%", display: "flex", flexDirection: "column", ...style }}>{children}</div>
);
// children see frames counted from `from`, and exist only while the sequence lasts
export const Sequence: React.FC<{ from: number; durationInFrames: number; children: React.ReactNode }> = ({ from, durationInFrames, children }) => {
  const frame = useContext(FrameCtx);
  if (frame < from || frame >= from + durationInFrames) return null;
  return <FrameCtx.Provider value={frame - from}>{children}</FrameCtx.Provider>;
};
export const Img: React.FC<React.ImgHTMLAttributes<HTMLImageElement>> = (p) => <img {...p} />;

export const interpolate = (x: number, [a, b]: [number, number], [c, d]: [number, number], o?: { extrapolateLeft?: "clamp"; extrapolateRight?: "clamp" }) => {
  let t = b === a ? 1 : (x - a) / (b - a);
  if (o?.extrapolateLeft === "clamp") t = Math.max(0, t);
  if (o?.extrapolateRight === "clamp") t = Math.min(1, t);
  return c + (d - c) * t;
};
// a number in [0, 1) that depends only on the seed
export const random = (seed: string | number) => {
  const s = String(seed); let h = 2166136261;
  for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); }
  h ^= h >>> 15; h = Math.imul(h, 2246822507); h ^= h >>> 13; h = Math.imul(h, 3266489909); h ^= h >>> 16;
  return (h >>> 0) / 4294967296;
};
