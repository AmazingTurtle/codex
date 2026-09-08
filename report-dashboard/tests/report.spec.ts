import { test, expect } from "@playwright/test";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import type { ReportData } from "../src/data/report-data";

const generatedAt = "2026-09-08T12:00:00Z";
function fixture(): ReportData {
  const usage: ReportData["usage"] = Array.from({ length: 90 }, (_, index) => ({
    timestamp: new Date(
      Date.parse("2026-08-10T12:00:00Z") +
        Math.floor(index / 3) * 86400000 +
        (index % 3) * 3600000,
    ).toISOString(),
    threadId: `thread-${index}`,
    responseId: `response-${index}`,
    accountId: index % 2 ? "personal@example.com" : "work@example.com",
    requestedModel: index % 3 ? "gpt-5.6-sol" : "gpt-6-astra",
    reportedModel: null,
    requestedServiceTier: index % 3 ? "priority" : "standard",
    reportedServiceTier: null,
    inputTokens: 12000 + ((index * 7231) % 20000),
    cachedInputTokens: 10000,
    cacheWriteInputTokens: 0,
    outputTokens: 1200 + ((index * 311) % 12000),
    reasoningOutputTokens: 600,
    source: index % 7 ? "exact" : "legacyApproximate",
  }));
  const limits: ReportData["limits"] = Array.from(
    { length: 80 },
    (_, index) => ({
      timestamp: new Date(
        Date.parse("2026-09-07T08:00:00Z") + Math.floor(index / 2) * 300000,
      ).toISOString(),
      accountId: index % 2 ? "personal@example.com" : "work@example.com",
      limitId: "weekly",
      windowSeconds: 604800,
      usedPercent: index < 40 ? 50 + index / 2 : (index - 40) / 2,
      resetsAt: "2026-09-14T08:00:00Z",
      source: "status",
    }),
  );
  return {
    schemaVersion: 1,
    generatedAt,
    usage,
    limits,
    resets: [
      {
        timestamp: "2026-09-07T09:40:00Z",
        accountId: "work@example.com",
        idempotencyKey: "reset",
        limitId: "weekly",
      },
    ],
    diagnostics: {
      filesDiscovered: 32,
      filesScanned: 32,
      fileReadErrors: 0,
      discoveryErrors: 0,
      telemetryReadErrors: 0,
      oversizedRecords: 0,
      recordsParsed: 171,
      malformedRecords: 1,
      duplicateUsageRecords: 2,
      conflictingUsageRecords: 0,
      exactUsageRecords: 77,
      legacyUsageRecords: 13,
    },
  };
}

let directory: string;
async function reportFile(data: unknown, name: string): Promise<string> {
  const [js, css] = await Promise.all([
    readFile(
      new URL("../../codex-rs/report/assets/dashboard.js", import.meta.url),
      "utf8",
    ),
    readFile(
      new URL("../../codex-rs/report/assets/dashboard.css", import.meta.url),
      "utf8",
    ),
  ]);
  const json = JSON.stringify(data)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e")
    .replace(/&/g, "\\u0026");
  const path = join(directory, name);
  await writeFile(
    path,
    `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>${css}</style></head><body><div id="root"></div><script id="report-data" type="application/json">${json}</script><script>${js.replace(/<\/script/gi, "<\\/script")}</script></body></html>`,
  );
  return pathToFileURL(path).href;
}

test.beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), "better-codex-dashboard-"));
});
test.afterAll(async () => {
  await rm(directory, { recursive: true, force: true });
});

test("ledger reverses chronological source rows and switches token ordering", async ({
  page,
}) => {
  const data = fixture();
  const sample = data.usage[0];
  if (!sample) throw new Error("Fixture missing usage");
  const usage = [
    {
      ...sample,
      timestamp: "2026-09-01T00:00:00Z",
      requestedModel: "earliest-largest",
      inputTokens: 90000,
    },
    { ...sample, timestamp: "2026-09-02T00:00:00Z", requestedModel: "middle" },
    { ...sample, timestamp: "2026-09-03T00:00:00Z", requestedModel: "latest" },
  ];
  await page.goto(await reportFile({ ...data, usage }, "ledger-order.html"));
  await expect(page.locator(".table-model")).toHaveText([
    "latest",
    "middle",
    "earliest-largest",
  ]);
  await page.getByRole("button", { name: "Newest first" }).click();
  await expect(page.locator(".table-model").first()).toHaveText(
    "earliest-largest",
  );
  await page.getByRole("button", { name: "Most tokens" }).click();
  await expect(page.locator(".table-model")).toHaveText([
    "latest",
    "middle",
    "earliest-largest",
  ]);
});

for (const width of [390, 768, 1440]) {
  for (const colorScheme of ["dark", "light"] as const) {
    test(`${colorScheme} ${width}px renders offline with functional filters`, async ({
      page,
    }, testInfo) => {
      await page.setViewportSize({ width, height: 1000 });
      await page.emulateMedia({ colorScheme, reducedMotion: "reduce" });
      const requests: string[] = [];
      const errors: string[] = [];
      page.on("request", (request) => {
        if (/^https?:/.test(request.url())) requests.push(request.url());
      });
      page.on("pageerror", (error) => errors.push(error.message));
      await page.context().setOffline(true);
      await page.goto(
        await reportFile(fixture(), `report-${width}-${colorScheme}.html`),
      );
      await expect(
        page.getByRole("heading", { name: "Token activity" }),
      ).toBeVisible();
      await expect(page.locator("html")).toHaveAttribute(
        "data-theme",
        colorScheme,
      );
      await expect.poll(() => page.locator("canvas").count()).toBe(5);
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      ).toBe(true);
      await page.screenshot({
        path: testInfo.outputPath("dashboard.png"),
        fullPage: true,
      });
      await page
        .getByLabel("Model", { exact: true })
        .selectOption("gpt-6-astra");
      await expect(page.locator(".metrics")).toContainText("30");
      await expect(page.locator(".limit-panel")).toHaveCount(2);
      await page
        .getByLabel("Account", { exact: true })
        .selectOption("work@example.com");
      await expect(page.locator(".limit-panel")).toHaveCount(1);
      await page.getByText("Banked reset annotations").click();
      await expect(page.locator(".reset-details li")).toBeVisible();
      await page.locator(".reset-details li button").click();
      await expect(page.getByRole("status")).toContainText(
        "Pinned banked reset",
      );
      await page.getByLabel("Reset filters").click();
      await page.getByLabel("Time bucket").selectOption("week");
      await page.getByRole("button", { name: "Reset zoom" }).click();
      await page
        .getByLabel("Attribution", { exact: true })
        .selectOption("reported");
      await expect(page.locator(".table-model").first()).toHaveText("Unknown");
      await page.getByRole("button", { name: /Theme:/ }).click();
      await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
      await page.getByRole("button", { name: /Theme:/ }).click();
      await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
      expect(requests).toStrictEqual([]);
      expect(errors).toStrictEqual([]);
    });
  }
}

test("empty, invalid and hostile data remain safe and understandable", async ({
  page,
}) => {
  const data = fixture();
  await page.goto(
    await reportFile(
      { ...data, usage: [], limits: [], resets: [] },
      "empty.html",
    ),
  );
  await expect(
    page.getByRole("heading", { name: "No activity in this view" }),
  ).toBeVisible();
  await page.goto(
    await reportFile({ ...data, schemaVersion: 999 }, "invalid.html"),
  );
  await expect(page.getByRole("alert")).toContainText("invalid");
  const first = data.usage[0];
  if (!first) throw new Error("Fixture missing usage");
  const hostile = "</script><img src=x onerror=alert(1)>";
  await page.goto(
    await reportFile(
      { ...data, usage: [{ ...first, reportedModel: hostile }] },
      "hostile.html",
    ),
  );
  await page
    .getByLabel("Attribution", { exact: true })
    .selectOption("reported");
  await expect(page.locator(".table-model")).toHaveText(hostile);
  await expect(page.locator("img")).toHaveCount(0);
});
