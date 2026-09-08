import type { ReportData, UsageRecord } from "./report-data";

export interface Filters {
  start: string;
  end: string;
  account: string;
  model: string;
  tier: string;
  attribution: "requested" | "reported";
}
export interface TokenTotals {
  responses: number;
  input: number;
  cached: number;
  cacheWrites: number;
  output: number;
  reasoning: number;
}
export interface ActivityBucket extends TokenTotals {
  timestamp: number;
}
export const UNKNOWN = "__unknown__";
export const compact = new Intl.NumberFormat("en", {
  notation: "compact",
  maximumFractionDigits: 1,
});
export const integer = new Intl.NumberFormat("en");
export const dateTime = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "short",
  timeZone: "UTC",
});

export function initialFilters(generatedAt: string): Filters {
  const end = new Date(generatedAt);
  const start = new Date(end);
  start.setUTCDate(start.getUTCDate() - 29);
  return {
    start: start.toISOString().slice(0, 10),
    end: end.toISOString().slice(0, 10),
    account: "",
    model: "",
    tier: "",
    attribution: "requested",
  };
}

export function inDateRange(timestamp: string, filters: Filters): boolean {
  const date = new Date(timestamp).toISOString().slice(0, 10);
  return (
    (!filters.start || date >= filters.start) &&
    (!filters.end || date <= filters.end)
  );
}

function matches(value: string | null, filter: string): boolean {
  return !filter || (value ?? UNKNOWN) === filter;
}

export function selectReport(data: ReportData, filters: Filters) {
  return {
    usage: data.usage.filter(
      (row) =>
        inDateRange(row.timestamp, filters) &&
        matches(row.accountId, filters.account) &&
        matches(
          attributionValue(row, "model", filters.attribution),
          filters.model,
        ) &&
        matches(
          attributionValue(row, "tier", filters.attribution),
          filters.tier,
        ),
    ),
    limits: data.limits.filter(
      (row) =>
        inDateRange(row.timestamp, filters) &&
        matches(row.accountId, filters.account),
    ),
    resets: data.resets.filter(
      (row) =>
        inDateRange(row.timestamp, filters) &&
        matches(row.accountId, filters.account),
    ),
  };
}

export function totals(rows: readonly UsageRecord[]): TokenTotals {
  return rows.reduce<TokenTotals>(
    (sum, row) => ({
      responses: sum.responses + 1,
      input: sum.input + row.inputTokens,
      cached: sum.cached + row.cachedInputTokens,
      cacheWrites: sum.cacheWrites + row.cacheWriteInputTokens,
      output: sum.output + row.outputTokens,
      reasoning: sum.reasoning + row.reasoningOutputTokens,
    }),
    {
      responses: 0,
      input: 0,
      cached: 0,
      cacheWrites: 0,
      output: 0,
      reasoning: 0,
    },
  );
}

/** UTC buckets bound chart geometry without approximating token totals. */
export function activity(
  rows: readonly UsageRecord[],
  bucket: "hour" | "day" | "week",
): ActivityBucket[] {
  const baseWidth =
    bucket === "hour" ? 3_600_000 : bucket === "day" ? 86_400_000 : 604_800_000;
  let first = Infinity;
  let last = -Infinity;
  for (const row of rows) {
    const time = Date.parse(row.timestamp);
    first = Math.min(first, time);
    last = Math.max(last, time);
  }
  const width =
    baseWidth * Math.max(1, Math.ceil((last - first) / baseWidth / 1999));
  const offset = bucket === "week" ? 345_600_000 : 0; // Monday-based UTC weeks.
  const grouped = new Map<number, UsageRecord[]>();
  for (const row of rows) {
    const time =
      Math.floor((Date.parse(row.timestamp) - offset) / width) * width + offset;
    const group = grouped.get(time);
    if (group) group.push(row);
    else grouped.set(time, [row]);
  }
  return [...grouped]
    .sort(([a], [b]) => a - b)
    .map(([timestamp, group]) => ({ timestamp, ...totals(group) }));
}

export function attributionValue(
  row: UsageRecord,
  key: "model" | "tier",
  attribution: Filters["attribution"],
): string | null {
  if (key === "model")
    return attribution === "requested" ? row.requestedModel : row.reportedModel;
  return attribution === "requested"
    ? row.requestedServiceTier
    : row.reportedServiceTier;
}

export function cachedInputShare(sum: TokenTotals): number | null {
  return sum.input === 0 ? null : (sum.cached / sum.input) * 100;
}

export function breakdown(
  rows: readonly UsageRecord[],
  key: "model" | "tier",
  attribution: Filters["attribution"],
) {
  const groups = new Map<string, number>();
  for (const row of rows) {
    const name = attributionValue(row, key, attribution) ?? "Unknown";
    groups.set(
      name,
      (groups.get(name) ?? 0) + row.inputTokens + row.outputTokens,
    );
  }
  return [...groups]
    .map(([name, value]) => ({ name, value }))
    .sort((a, b) => b.value - a.value);
}

export function options(
  values: readonly (string | null)[],
): { value: string; label: string }[] {
  return [...new Set(values)]
    .sort((a, b) => (a ?? "").localeCompare(b ?? ""))
    .map((value) => ({ value: value ?? UNKNOWN, label: value ?? "Unknown" }));
}
