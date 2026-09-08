import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  workers: 1,
  use: {
    browserName: "chromium",
    headless: true,
    launchOptions: process.env.REPORT_TEST_BROWSER
      ? { executablePath: process.env.REPORT_TEST_BROWSER }
      : {},
  },
});
