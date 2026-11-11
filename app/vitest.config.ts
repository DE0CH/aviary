import { defineConfig } from "vitest/config";

// The shared config of a run from paseo/ and of the packages without their own (protocol, ...). React's test APIs need
// its development build, also when the shell exports NODE_ENV=production.
process.env.NODE_ENV = "test";

export default defineConfig({});
