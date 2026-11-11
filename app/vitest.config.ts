import { defineConfig } from "vitest/config";

// A run started from paseo/ (e.g. `npx vitest run packages/app/src/...`) uses each package's own config. React's test
// APIs need its development build, also when the shell exports NODE_ENV=production.
process.env.NODE_ENV = "test";

export default defineConfig({
  test: {
    projects: ["packages/*"],
  },
});
