// Motion graphics drawn over the footage: animated emoji, stickers, confetti, arrows, hand-drawn circles, spotlights,
// speed lines, flashes, light leaks, sparkles, letterbox bars, a progress bar.
import React from "react";
import { AbsoluteFill, Img, Sequence, useCurrentFrame, useVideoConfig } from "./rt";
import lottie from "lottie-web";
import { backOut, clamp, ease, easeOut, FONTS, rnd, Theme } from "./theme";

export type Graphic = {
  from: number; to: number; type: string;
  x?: number; y?: number; size?: number; w?: number; h?: number; r?: number;
  color?: string; colors?: string[]; emoji?: string; lottie?: string; url?: string;
  animate?: string; loop?: string; rotate?: number; count?: number; direction?: string; angle?: number; seed?: number; opacity?: number;
};
const PALETTE = ["#ff3b30", "#ffcc00", "#34c759", "#0a84ff", "#ff2d92", "#ffffff", "#af52de", "#ff9500"];

const entrance = (kind: string | undefined, t: number, tout: number, fps: number) => {
  const o = clamp(t / (fps * 0.1)) * clamp(tout / (fps * 0.2));
  switch (kind || "pop") {
    case "none": return { o: 1, tr: "" };
    case "fade": return { o: clamp(t / (fps * 0.25)) * clamp(tout / (fps * 0.2)), tr: "" };
    case "drop": return { o, tr: `translateY(${(backOut(t / (fps * 0.45)) - 1) * 600}px)` };
    case "spin": { const e = easeOut(t / (fps * 0.45)); return { o, tr: `rotate(${(1 - e) * -270}deg) scale(${0.2 + 0.8 * e})` }; }
    case "slide-left": return { o, tr: `translateX(${(1 - easeOut(t / (fps * 0.35))) * 500}px)` };
    case "slide-right": return { o, tr: `translateX(${(easeOut(t / (fps * 0.35)) - 1) * 500}px)` };
    default: return { o, tr: `scale(${0.2 + 0.8 * backOut(t / (fps * 0.35))})` };
  }
};
const looping = (kind: string | undefined, s: number) => {
  switch (kind) {
    case "wiggle": return `rotate(${Math.sin(s * 9) * 8}deg)`;
    case "pulse": return `scale(${1 + Math.sin(s * 7) * 0.1})`;
    case "float": return `translateY(${Math.sin(s * 3) * 18}px)`;
    case "shake": return `translate(${Math.sin(s * 47) * 6}px,${Math.cos(s * 53) * 6}px)`;
    case "spin": return `rotate(${s * 180}deg)`;
    case "bounce": return `translateY(${-Math.abs(Math.sin(s * 6)) * 40}px)`;
    default: return "";
  }
};

// A Lottie animation shown at the frame the video is at (looping), so every render of a frame is the same picture.
const Lottie: React.FC<{ animationData: any; loop?: boolean; style?: React.CSSProperties }> = ({ animationData, style }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const box = React.useRef<HTMLDivElement>(null), anim = React.useRef<any>(null);
  React.useLayoutEffect(() => {
    if (!box.current || !animationData) return;
    anim.current = lottie.loadAnimation({ container: box.current, renderer: "svg", loop: false, autoplay: false, animationData: JSON.parse(JSON.stringify(animationData)) });
    return () => { anim.current?.destroy(); anim.current = null; };
  }, [animationData]);
  React.useLayoutEffect(() => {
    const a = anim.current; if (!a) return;
    const total = Math.max(1, Math.floor(a.totalFrames)), rate = (animationData?.fr || 30) / fps;
    a.goToAndStop(Math.floor(frame * rate) % total, true);
  });
  return <div ref={box} style={style} />;
};
// a warm film light leak: soft blobs of light that sweep across and fade
const LightLeak: React.FC<{ durationInFrames: number; seed?: number; hueShift?: number }> = ({ durationInFrames, seed = 3, hueShift = 0 }) => {
  const t = useCurrentFrame() / Math.max(1, durationInFrames), bell = Math.sin(Math.PI * clamp(t));
  const blobs = [0, 1, 2].map((i) => {
    const x = (rnd(seed, i) * 60 + 20) + (t - 0.5) * (40 + rnd(seed, i + 9) * 60) * (i % 2 ? 1 : -1), y = rnd(seed, i + 20) * 80 + 10;
    return `radial-gradient(ellipse ${50 + rnd(seed, i + 30) * 40}% ${40 + rnd(seed, i + 40) * 50}% at ${x}% ${y}%, hsla(${(28 + i * 14 + hueShift) % 360}, 100%, ${62 - i * 6}%, ${0.95 * bell}), transparent 70%)`;
  });
  return <AbsoluteFill style={{ background: blobs.join(","), filter: "blur(30px)" }} />;
};

const Emoji: React.FC<{ emoji?: string; data?: any; px: number }> = ({ emoji, data, px }) =>
  data ? <Lottie animationData={data} loop style={{ width: px, height: px }} />
    : <div style={{ fontFamily: FONTS.hei.replace("'Chiron Hei HK'", "'Noto Color Emoji','Chiron Hei HK'"), fontSize: px * 0.82, lineHeight: `${px}px`, width: px, height: px, textAlign: "center" }}>{emoji}</div>;

const One: React.FC<{ g: Graphic; th: Theme; lotties: Record<string, any> }> = ({ g, th, lotties }) => {
  const t = useCurrentFrame();                       // frames since this graphic started
  const { width: W, height: H, fps } = useVideoConfig();
  const dur = g.to - g.from, tout = dur - t, s = t / fps;
  const x = g.x ?? 50, y = g.y ?? 50, color = g.color || th.accent;
  const seed = g.seed ?? g.from;
  const at = (node: React.ReactNode, anim = entrance(g.animate, t, tout, fps)) => (
    <div style={{ position: "absolute", left: `${x}%`, top: `${y}%`, transform: "translate(-50%,-50%)" }}>
      <div style={{ transform: `${anim.tr} ${looping(g.loop, s)} rotate(${g.rotate || 0}deg)`, opacity: anim.o * (g.opacity ?? 1) }}>{node}</div>
    </div>);
  switch (g.type) {
    case "emoji": return at(<Emoji emoji={g.emoji} data={g.lottie ? lotties[g.lottie] : null} px={((g.size ?? 22) / 100) * W} />);
    case "lottie": return at(<Lottie animationData={lotties[g.lottie!]} loop style={{ width: ((g.size ?? 40) / 100) * W }} />);
    case "image": return at(<Img src={g.url!} style={{ width: ((g.size ?? 30) / 100) * W, display: "block" }} />);
    case "emoji-rain": {
      const n = g.count ?? 18, up = g.direction === "up", px = ((g.size ?? 11) / 100) * W;
      return <AbsoluteFill style={{ opacity: clamp(tout / (fps * 0.3)) }}>{Array.from({ length: n }, (_, i) => {
        const delay = rnd(seed, i) * dur * 0.5, life = t - delay; if (life < 0) return null;
        const speed = (0.55 + rnd(seed, i + 100) * 0.6) * H / fps * 0.9, travel = life * speed;
        const left = rnd(seed, i + 200) * 92 + 4, sway = Math.sin(life / 9 + i) * 30;
        return <div key={i} style={{ position: "absolute", left: `${left}%`, top: up ? H + px - travel : -px * 1.5 + travel, transform: `translateX(${sway - px / 2}px) rotate(${Math.sin(life / 12 + i) * 25}deg)` }}><Emoji emoji={g.emoji} data={g.lottie ? lotties[g.lottie] : null} px={px * (0.7 + rnd(seed, i + 300) * 0.6)} /></div>;
      })}</AbsoluteFill>;
    }
    case "confetti": {
      const n = g.count ?? 140, burst = g.x != null || g.y != null, cols = g.colors || PALETTE;
      return <AbsoluteFill style={{ opacity: clamp(tout / (fps * 0.4)) }}>{Array.from({ length: n }, (_, i) => {
        const a = rnd(seed, i) * Math.PI * 2, v = (0.35 + rnd(seed, i + 1000) * 0.65);
        const sz = W * (0.012 + rnd(seed, i + 2000) * 0.016), grav = 0.5 * 2200 * s * s;
        const px = burst ? (x / 100) * W + Math.cos(a) * v * W * 0.9 * (1 - Math.exp(-s * 2.2)) : rnd(seed, i + 3000) * W + Math.sin(s * 3 + i) * 40;
        const py = burst ? (y / 100) * H + Math.sin(a) * v * W * 0.9 * (1 - Math.exp(-s * 2.2)) + grav * 0.45 : -40 - rnd(seed, i + 4000) * H * 0.6 + s * H * (0.45 + v * 0.5);
        return <div key={i} style={{ position: "absolute", left: px, top: py, width: sz, height: sz * (i % 3 === 0 ? 0.45 : 1), borderRadius: i % 4 === 1 ? "50%" : 2, background: cols[i % cols.length], transform: `rotate(${s * (200 + rnd(seed, i + 5000) * 500) + i * 40}deg) scaleX(${Math.cos(s * 9 + i)})` }} />;
      })}</AbsoluteFill>;
    }
    case "arrow": {
      // points AT (x,y); "angle" is the direction it comes from (0 = from the right, 90 = from below, 180 = from the left, 270 = from above)
      const len = ((g.size ?? 26) / 100) * W, ang = g.angle ?? 135, e = easeOut(t / (fps * 0.3));
      const bob = Math.sin(s * 8) * len * 0.07, stroke = len * 0.11;
      return <div style={{ position: "absolute", left: `${x}%`, top: `${y}%`, width: 0, height: 0, transform: `rotate(${ang}deg)`, opacity: clamp(t / 3) * clamp(tout / (fps * 0.2)) }}>
        <svg width={len * 1.3} height={len * 0.8} viewBox={`0 0 ${len * 1.3} ${len * 0.8}`} style={{ position: "absolute", left: len * 0.06 + bob, top: -len * 0.4, overflow: "visible" }}>
          <g fill="none" stroke="#111111" strokeWidth={stroke * 1.9} strokeLinecap="round" strokeLinejoin="round">
            <path d={`M ${len * 1.15} ${len * 0.4} L ${len * 0.12} ${len * 0.4}`} pathLength={1} strokeDasharray={1} strokeDashoffset={1 - e} />
            <path d={`M ${len * 0.42} ${len * 0.12} L ${len * 0.1} ${len * 0.4} L ${len * 0.42} ${len * 0.68}`} opacity={e > 0.6 ? 1 : 0} /></g>
          <g fill="none" stroke={color} strokeWidth={stroke} strokeLinecap="round" strokeLinejoin="round">
            <path d={`M ${len * 1.15} ${len * 0.4} L ${len * 0.12} ${len * 0.4}`} pathLength={1} strokeDasharray={1} strokeDashoffset={1 - e} />
            <path d={`M ${len * 0.42} ${len * 0.12} L ${len * 0.1} ${len * 0.4} L ${len * 0.42} ${len * 0.68}`} opacity={e > 0.6 ? 1 : 0} /></g>
        </svg></div>;
    }
    case "circle": {
      // a hand-drawn ring around the area centred on (x,y), w x h in percent of the frame
      const w = ((g.w ?? 40) / 100) * W, h = ((g.h ?? g.w ?? 40) / 100) * (g.h != null ? H : W), e = ease(t / (fps * 0.45)), sw = Math.max(6, W * 0.012);
      const ring = (k: number, rot: number) => <ellipse cx={w / 2} cy={h / 2} rx={w / 2 - sw - k} ry={h / 2 - sw + k} transform={`rotate(${rot} ${w / 2} ${h / 2})`} pathLength={1} strokeDasharray={1} strokeDashoffset={1 - e} />;
      return <svg width={w} height={h} style={{ position: "absolute", left: (x / 100) * W - w / 2, top: (y / 100) * H - h / 2, overflow: "visible", opacity: clamp(tout / (fps * 0.2)) }}>
        <g fill="none" stroke="#111111" strokeWidth={sw * 1.8} strokeLinecap="round">{ring(0, -4)}</g>
        <g fill="none" stroke={color} strokeWidth={sw} strokeLinecap="round">{ring(0, -4)}{ring(sw * 0.8, 3)}</g></svg>;
    }
    case "spotlight": {
      const r = ((g.r ?? 26) / 100) * W, e = ease(t / (fps * 0.35)) * clamp(tout / (fps * 0.3));
      return <AbsoluteFill style={{ background: `radial-gradient(circle at ${x}% ${y}%, transparent ${r}px, rgba(0,0,0,${0.78 * e}) ${r * 1.25}px)` }} />;
    }
    case "speedlines": {
      const n = 46, e = clamp(t / 4) * clamp(tout / 6), R = Math.hypot(W, H);
      return <svg width={W} height={H} style={{ position: "absolute", inset: 0, opacity: e * (g.opacity ?? 0.85) }}>{Array.from({ length: n }, (_, i) => {
        const a = (i / n) * Math.PI * 2 + rnd(`${seed}-${Math.floor(t / 2)}`, i) * 0.12, inner = (0.3 + rnd(`${seed}-${Math.floor(t / 2)}`, i + 99) * 0.22) * W, wd = 0.008 + rnd(seed, i) * 0.02;
        const cx = (x / 100) * W, cy = (y / 100) * H;
        const p = (rr: number, aa: number) => `${cx + Math.cos(aa) * rr},${cy + Math.sin(aa) * rr}`;
        return <polygon key={i} points={`${p(inner, a)} ${p(R, a - wd)} ${p(R, a + wd)}`} fill={g.color || "#ffffff"} />;
      })}</svg>;
    }
    case "flash": return <AbsoluteFill style={{ background: g.color || "#ffffff", opacity: Math.pow(1 - clamp(t / Math.max(1, dur)), 1.6) * (g.opacity ?? 0.95) }} />;
    case "light-leak": return <AbsoluteFill style={{ mixBlendMode: "screen", opacity: g.opacity ?? 0.8 }}><LightLeak durationInFrames={dur} seed={g.seed ?? 3} hueShift={g.rotate ?? 0} /></AbsoluteFill>;
    case "sparkles": {
      const n = g.count ?? 14, bw = ((g.w ?? 60) / 100) * W, bh = ((g.h ?? 30) / 100) * H;
      return <div style={{ position: "absolute", left: (x / 100) * W - bw / 2, top: (y / 100) * H - bh / 2, width: bw, height: bh, opacity: clamp(tout / (fps * 0.2)) }}>{Array.from({ length: n }, (_, i) => {
        const ph = (s * (1.2 + rnd(seed, i) * 1.4) + rnd(seed, i + 50)) % 1, sc = Math.sin(ph * Math.PI), sz = W * (0.03 + rnd(seed, i + 70) * 0.045);
        return <svg key={i} viewBox="0 0 24 24" width={sz} height={sz} style={{ position: "absolute", left: `${rnd(seed, i + 10) * 100}%`, top: `${rnd(seed, i + 20) * 100}%`, transform: `translate(-50%,-50%) scale(${sc}) rotate(${ph * 90}deg)` }}>
          <path d="M12 0 C13 8 16 11 24 12 C16 13 13 16 12 24 C11 16 8 13 0 12 C8 11 11 8 12 0Z" fill={i % 3 ? (g.color || "#ffffff") : th.accent} /></svg>;
      })}</div>;
    }
    case "bars": {
      const hgt = H * ((g.size ?? 9) / 100) * ease(t / (fps * 0.4)) * clamp(tout / (fps * 0.3));
      return <AbsoluteFill><div style={{ position: "absolute", top: 0, left: 0, right: 0, height: hgt, background: g.color || "#000000" }} /><div style={{ position: "absolute", bottom: 0, left: 0, right: 0, height: hgt, background: g.color || "#000000" }} /></AbsoluteFill>;
    }
    case "vignette": return <AbsoluteFill style={{ background: "radial-gradient(ellipse at center, transparent 45%, rgba(0,0,0,.65) 100%)", opacity: (g.opacity ?? 1) * clamp(t / (fps * 0.3)) }} />;
    case "tint": return <AbsoluteFill style={{ background: color, opacity: (g.opacity ?? 0.25) * clamp(t / (fps * 0.2)) * clamp(tout / (fps * 0.2)), mixBlendMode: "overlay" }} />;
    default: return null;
  }
};

export const Graphics: React.FC<{ graphics: Graphic[]; th: Theme; lotties: Record<string, any> }> = ({ graphics, th, lotties }) => (
  <AbsoluteFill>{graphics.map((g, i) => <Sequence key={i} from={g.from} durationInFrames={Math.max(1, g.to - g.from)} layout="none"><One g={g} th={th} lotties={lotties} /></Sequence>)}</AbsoluteFill>
);

export const ProgressBar: React.FC<{ th: Theme; color?: string }> = ({ th, color }) => {
  const frame = useCurrentFrame();
  const { durationInFrames, height, width } = useVideoConfig();
  const p = frame / Math.max(1, durationInFrames - 1), h = Math.max(8, height * 0.006);
  return <div style={{ position: "absolute", left: 0, bottom: 0, width, height: h, background: "rgba(255,255,255,.18)" }}>
    <div style={{ width: `${p * 100}%`, height: "100%", background: color || `linear-gradient(90deg, ${th.accent}, ${th.accent2})` }} /></div>;
};

// animated backgrounds for cards and for clips shown inset
export const Backdrop: React.FC<{ kind: string; color: string; th: Theme }> = ({ kind, color, th }) => {
  const frame = useCurrentFrame();
  const { width: W, height: H, fps } = useVideoConfig();
  const s = frame / fps;
  const base: React.CSSProperties = { background: color };
  if (kind === "gradient") return <AbsoluteFill style={{ background: `linear-gradient(${140 + Math.sin(s) * 25}deg, ${color}, ${th.accent2} 130%)` }} />;
  if (kind === "dots") return <AbsoluteFill style={{ ...base, backgroundImage: `radial-gradient(rgba(255,255,255,.55) 18%, transparent 19%)`, backgroundSize: `${W * 0.09}px ${W * 0.09}px`, backgroundPosition: `${s * 30}px ${s * 30}px` }} />;
  if (kind === "grid") return <AbsoluteFill style={{ ...base, backgroundImage: `linear-gradient(rgba(255,255,255,.2) 3px, transparent 3px), linear-gradient(90deg, rgba(255,255,255,.2) 3px, transparent 3px)`, backgroundSize: `${W * 0.1}px ${W * 0.1}px`, backgroundPosition: `0 ${s * 40}px` }} />;
  if (kind === "rays") return <AbsoluteFill style={base}><div style={{ position: "absolute", left: "50%", top: "50%", width: H * 1.6, height: H * 1.6, transform: `translate(-50%,-50%) rotate(${s * 14}deg)`, background: `repeating-conic-gradient(rgba(255,255,255,.16) 0 9deg, transparent 9deg 18deg)` }} /><AbsoluteFill style={{ background: `radial-gradient(circle, transparent 20%, ${color} 95%)`, opacity: 0.55 }} /></AbsoluteFill>;
  if (kind === "rings") return <AbsoluteFill style={{ ...base, alignItems: "center", justifyContent: "center" }}>
    <AbsoluteFill style={{ background: `radial-gradient(circle at 50% 50%, ${th.accent}33, transparent 60%)` }} />
    {[0, 1, 2, 3].map((i) => { const ph = ((s * 0.35 + i / 4) % 1); return <div key={i} style={{ position: "absolute", width: W * (0.4 + ph * 1.2), height: W * (0.4 + ph * 1.2), borderRadius: "50%", border: `3px solid ${i % 2 ? th.accent2 : th.accent}`, opacity: (1 - ph) * 0.55 }} />; })}</AbsoluteFill>;
  return <AbsoluteFill style={base} />;
};
