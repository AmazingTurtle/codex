import { describe, expect, it } from "vitest";
import {
  activity,
  cachedInputShare,
  initialFilters,
  selectReport,
  totals,
} from "./aggregate";
import { parseReport } from "./report-data";
import type { ReportData, UsageRecord } from "./report-data";

const row: UsageRecord = {
  timestamp: "2026-09-01T12:00:00Z",
  threadId: "thread",
  responseId: "response",
  accountId: "account",
  requestedModel: "model",
  requestedServiceTier: "priority",
  reportedModel: null,
  reportedServiceTier: null,
  inputTokens: 100,
  cachedInputTokens: 80,
  cacheWriteInputTokens: 10,
  outputTokens: 20,
  reasoningOutputTokens: 12,
  source: "exact",
};
const data: ReportData = {
  schemaVersion: 1,
  generatedAt: "2026-09-08T00:00:00Z",
  usage: [row],
  limits: [
    {
      timestamp: row.timestamp,
      accountId: "account",
      limitId: "weekly",
      windowSeconds: 604800,
      usedPercent: 20,
      resetsAt: null,
      source: "status",
    },
  ],
  resets: [
    {
      timestamp: row.timestamp,
      accountId: "account",
      limitId: "weekly",
      idempotencyKey: "reset",
    },
  ],
  diagnostics: {
    filesDiscovered: 1,
    filesScanned: 1,
    fileReadErrors: 0,
    discoveryErrors: 0,
    telemetryReadErrors: 0,
    oversizedRecords: 0,
    recordsParsed: 1,
    malformedRecords: 0,
    duplicateUsageRecords: 0,
    conflictingUsageRecords: 0,
    exactUsageRecords: 1,
    legacyUsageRecords: 0,
  },
};

describe("report filtering and token accounting", () => {
  it("computes cached-input share from input only, never output", () => {
    expect(
      cachedInputShare(
        totals([
          {
            ...row,
            inputTokens: 200,
            cachedInputTokens: 100,
            outputTokens: 900,
          },
        ]),
      ),
    ).toBe(50);
    expect(cachedInputShare(totals([]))).toBeNull();
  });
  it("does not treat requested attribution as server confirmation", () => {
    expect(
      selectReport(data, {
        ...initialFilters(data.generatedAt),
        attribution: "reported",
        model: "model",
      }).usage,
    ).toStrictEqual([]);
  });
  it("bounds long hourly charts without losing token totals", () => {
    const rows = Array.from({ length: 10000 }, (_, index) => ({
      ...row,
      timestamp: new Date(index * 3600000).toISOString(),
    }));
    const chart = activity(rows, "hour");
    expect(chart.length).toBeLessThanOrEqual(2000);
    expect(chart.reduce((sum, entry) => sum + entry.input, 0)).toBe(1000000);
  });
  it("preserves token subsets without inflating input or output", () => {
    expect(totals([row, row])).toStrictEqual({
      responses: 2,
      input: 200,
      cached: 160,
      cacheWrites: 20,
      output: 40,
      reasoning: 24,
    });
  });
  it("keeps account limits and reset events when model/tier filters exclude usage", () => {
    expect(
      selectReport(data, {
        ...initialFilters(data.generatedAt),
        model: "other",
        tier: "standard",
      }),
    ).toStrictEqual({ usage: [], limits: data.limits, resets: data.resets });
    expect(
      selectReport(data, {
        ...initialFilters(data.generatedAt),
        account: "other",
      }),
    ).toStrictEqual({ usage: [], limits: [], resets: [] });
  });
  it("includes the entire final day and excludes the next day", () => {
    const usage = [
      { ...row, timestamp: "2026-09-08T23:59:59Z" },
      { ...row, timestamp: "2026-09-09T00:00:00Z" },
    ];
    expect(
      selectReport({ ...data, usage }, initialFilters(data.generatedAt)).usage,
    ).toStrictEqual([usage[0]]);
  });
  it("sorts out-of-order observations into exact UTC buckets", () => {
    expect(
      activity(
        [{ ...row, timestamp: "2026-09-02T23:00:00Z" }, row, row],
        "day",
      ),
    ).toStrictEqual([
      {
        timestamp: Date.parse("2026-09-01T00:00:00Z"),
        responses: 2,
        input: 200,
        cached: 160,
        cacheWrites: 20,
        output: 40,
        reasoning: 24,
      },
      {
        timestamp: Date.parse("2026-09-02T00:00:00Z"),
        responses: 1,
        input: 100,
        cached: 80,
        cacheWrites: 10,
        output: 20,
        reasoning: 12,
      },
    ]);
  });
});

describe("embedded JSON boundary", () => {
  it("rejects unsafe, negative, malformed, and impossible token values", () => {
    for (const inputTokens of [
      -1,
      Number.MAX_SAFE_INTEGER + 1,
      Infinity,
      "100",
    ]) {
      expect(() =>
        parseReport({ ...data, usage: [{ ...row, inputTokens }] }),
      ).toThrow();
    }
    expect(() =>
      parseReport({ ...data, usage: [{ ...row, cachedInputTokens: 101 }] }),
    ).toThrow();
    expect(() => parseReport({ ...data, generatedAt: "yesterday" })).toThrow();
    expect(() => parseReport({ ...data, schemaVersion: 2 })).toThrow();
  });
  it("preserves unknown attribution and untrusted text as data", () => {
    const usage = [
      {
        ...row,
        reportedModel: "</script><img src=x onerror=alert(1)>",
        accountId: null,
      },
    ];
    expect(parseReport({ ...data, usage }).usage).toStrictEqual(usage);
  });
});
