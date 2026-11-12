import { useSyncExternalStore } from "react";

/**
 * The product's name. The app's text says "YYC AI" (i18n/resources/yyc.ts); a deployment with another name says so
 * in /api/jarvis/me (`product`: hermit's `product.name`), and the app then names itself with that everywhere: the
 * translations through an i18next post-processor (i18n/i18next.ts), the few literal mentions through
 * `productName()` / `useProductName()`.
 */
export const DEFAULT_PRODUCT_NAME = "YYC AI";

/**
 * The name the server wrote into the page (pigeon sets the `apple-mobile-web-app-title` meta to the product's name):
 * known before /api/jarvis/me answers and when nobody is signed in.
 */
function pageProductName(): string | null {
  if (typeof document === "undefined") return null;
  const name = document
    .querySelector('meta[name="apple-mobile-web-app-title"]')
    ?.getAttribute("content")
    ?.trim();
  return name || null;
}

let current = pageProductName() ?? DEFAULT_PRODUCT_NAME;
const listeners = new Set<() => void>();

export function productName(): string {
  return current;
}

/** The name the server gave; nothing changes for an empty one or the name already in use. */
export function setProductName(name: unknown): void {
  if (typeof name !== "string") return;
  const next = name.trim();
  if (!next || next === current) return;
  current = next;
  for (const listener of listeners) listener();
}

/** Called with every change of the name (the translations re-render on it). */
export function onProductNameChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** `text` with the app's own name replaced by the product's. */
export function withProductName(text: string, name: string = current): string {
  return name === DEFAULT_PRODUCT_NAME ? text : text.replaceAll(DEFAULT_PRODUCT_NAME, name);
}

export function useProductName(): string {
  return useSyncExternalStore(onProductNameChange, productName, productName);
}
