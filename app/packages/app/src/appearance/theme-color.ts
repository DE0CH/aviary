/**
 * The page's `theme-color`: what a phone's browser paints its bar with and what an installed web app
 * (Chrome's "Install app") paints the status bar with. public/index.html starts with one meta per system scheme;
 * the app's own theme wins over the system's, so every one of them takes the theme's background.
 */
export function setThemeColor(doc: Pick<Document, "querySelectorAll">, color: string): void {
  for (const meta of Array.from(doc.querySelectorAll('meta[name="theme-color"]'))) {
    if (meta.getAttribute("content") !== color) meta.setAttribute("content", color);
  }
}
