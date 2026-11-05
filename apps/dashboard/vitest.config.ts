import { defineConfig } from "vitest/config";

// The Worker's tests run in Node, without the Cloudflare plugin from vite.config.ts.
export default defineConfig({
  test: { include: ["worker/**/*.test.ts", "src/**/*.test.{ts,tsx}"] },
});
