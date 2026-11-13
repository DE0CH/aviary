// The page's script: draws the plan `edit` wrote into window.__PLAN at whatever time HyperFrames seeks to.
import React from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { Stage } from "./rt";
import { CaptionStyle, theme } from "./theme";
import { Caption, Captions, TextItem, Texts } from "./text";
import { Graphic, Graphics, ProgressBar } from "./graphics";
import { applyItems, Item, Items } from "./stage";

type Plan = {
  width: number; height: number; fps: number; totalFrames: number;
  theme?: string; accent?: string; accent2?: string; font?: string;
  items: Item[]; captions: Caption[]; texts: TextItem[]; graphics: Graphic[];
  lotties: Record<string, any>; captionStyle: CaptionStyle; progressBar?: boolean | { color?: string };
};
const plan: Plan = (window as any).__PLAN;
const th = theme(plan.theme, { accent: plan.accent, accent2: plan.accent2, font: plan.font } as any);
const config = { fps: plan.fps, width: plan.width, height: plan.height, durationInFrames: plan.totalFrames };
const root = createRoot(document.getElementById("overlay")!);

const draw = (seconds: number) => {
  const frame = Math.max(0, Math.min(plan.totalFrames - 1, Math.floor(seconds * plan.fps + 1e-4)));
  applyItems(plan.items, frame, plan.width, plan.fps);
  flushSync(() => root.render(
    <Stage frame={frame} config={config}>
      <Items items={plan.items} th={th} lotties={plan.lotties || {}} />
      <Graphics graphics={plan.graphics || []} th={th} lotties={plan.lotties || {}} />
      <Texts texts={plan.texts || []} th={th} />
      <Captions captions={plan.captions || []} style={plan.captionStyle || {}} th={th} />
      {plan.progressBar ? <ProgressBar th={th} color={typeof plan.progressBar === "object" ? plan.progressBar.color : undefined} /> : null}
    </Stage>));
};

// One linear tween over the whole video; its target's setter draws, so every seek, however it is made, redraws.
const clock = { _t: 0, get t() { return this._t; }, set t(v: number) { this._t = v; draw(v); } };
const gsap = (window as any).gsap;
const tl = gsap.timeline({ paused: true });
tl.fromTo(clock, { t: 0 }, { t: plan.totalFrames / plan.fps, duration: plan.totalFrames / plan.fps, ease: "none" }, 0);
(window as any).__timelines = (window as any).__timelines || {};
(window as any).__timelines["main"] = tl;
tl.seek(0);
draw(0);
