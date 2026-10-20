// Small inline icons (stroke, currentColor). Decorative: every button that uses one also has a label.
import type { ToolIcon } from "./tools.ts";

const P: Record<string, string> = {
  menu: "M4 6h16M4 12h16M4 18h16",
  plus: "M12 5v14M5 12h14",
  send: "M5 12h13M13 6l6 6-6 6",
  stop: "M7 7h10v10H7z",
  clip: "M16.5 7.5l-7.8 7.8a2.2 2.2 0 01-3.1-3.1l8.5-8.5a3.6 3.6 0 015.1 5.1l-8.8 8.8a5 5 0 01-7.1-7.1L11 2.8",
  close: "M6 6l12 12M18 6L6 18",
  dots: "M5 12h.01M12 12h.01M19 12h.01",
  copy: "M9 9h10v10H9zM5 15V5h10",
  check: "M5 12.5l4.5 4.5L19 7",
  chevron: "M9 6l6 6-6 6",
  search: "M11 18a7 7 0 100-14 7 7 0 000 14zM20 20l-4-4",
  news: "M4 5h13v14H6a2 2 0 01-2-2zM17 9h3v8a2 2 0 01-2 2M7 9h7M7 13h7M7 16h4",
  social: "M7 10a3 3 0 100-6 3 3 0 000 6zM17 12a3 3 0 100-6 3 3 0 000 6zM2 20c0-3 2.2-5 5-5s5 2 5 5M13 20c.3-2.5 2-4 4-4 2.8 0 5 2 5 4",
  page: "M6 3h8l4 4v14H6zM14 3v4h4M9 12h6M9 16h6",
  terminal: "M4 5h16v14H4zM7 9l3 3-3 3M12 15h5",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  data: "M4 19V9M10 19V5M16 19v-7M22 19H2",
  globe: "M12 21a9 9 0 100-18 9 9 0 000 18zM3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9S9.5 5.6 12 3z",
  tool: "M14.7 6.3a4 4 0 00-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 005.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  alert: "M12 8v5M12 16.5h.01M10.3 3.9L2.4 18a2 2 0 001.7 3h15.8a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z",
  lock: "M7 11V8a5 5 0 0110 0v3M5 11h14v10H5z",
};

export function Icon({ name, size = 20 }: { name: keyof typeof P | ToolIcon; size?: number }) {
  return (
    <svg class="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d={P[name] ?? P.tool} />
    </svg>
  );
}
