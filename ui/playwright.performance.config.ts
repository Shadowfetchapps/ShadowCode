import { defineConfig } from "@playwright/test";
import base from "./playwright.config";

// Opt in explicitly. Host-sensitive latency targets never gate the default suite.
export default defineConfig({
  ...base,
  testMatch: /.*\.perf\.ts/,
  timeout: 120_000,
  use: { ...base.use, trace: "off", screenshot: "off", video: "off" },
});
