import { render } from "preact";
import "./style.css";
import { App } from "./app.tsx";

// The composer sits at the bottom of the visible viewport, above the on-screen keyboard (iOS Safari doesn't resize
// the layout viewport for the keyboard, so follow visualViewport).
function trackViewport() {
  const vv = window.visualViewport;
  const set = () => document.documentElement.style.setProperty("--app-h", `${Math.round(vv ? vv.height : window.innerHeight)}px`);
  set();
  vv?.addEventListener("resize", set);
  window.addEventListener("resize", set);
}
trackViewport();

render(<App />, document.getElementById("app")!);
