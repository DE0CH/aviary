// Which pages have a message box, for public/index.html's early message box (docs/paseo-ui.md "Loading"): the page's
// inline script holds the same pattern (a test keeps the two equal, and classifies every route file in src/app).
// "/" and "/h/<host>" redirect to a conversation or a new one.
export const COMPOSER_ROUTE_PATTERN = "^/(new|h/[^/]+(/(workspace|agent)/[^/]+)?)?$";
/** Conversation pages: the message box sits at the bottom, not in the middle. */
export const CONVERSATION_ROUTE_PATTERN = "^/h/[^/]+/(workspace|agent)/[^/]+$";

const composerRoute = new RegExp(COMPOSER_ROUTE_PATTERN);

/** The path with trailing slashes dropped ("/" stays "/"). */
export function normalizeEarlyInputPath(pathname: string): string {
  return pathname.replace(/\/+$/, "") || "/";
}

export function isComposerRoute(pathname: string): boolean {
  return composerRoute.test(normalizeEarlyInputPath(pathname));
}
