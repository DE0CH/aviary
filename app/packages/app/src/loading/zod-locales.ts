// Stands in for zod's locales index on web (metro.config.cjs): zod re-exports every language's error messages as
// `z.locales`, which nothing here reads; it configures English itself (docs/paseo-ui.md "Loading").
export { default as en } from "zod/v4/locales/en.js";
