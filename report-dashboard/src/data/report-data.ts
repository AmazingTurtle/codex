import { z } from "zod";
import type {
  LimitRecord,
  ReportData,
  ResetRecord,
  UsageRecord,
} from "../../../codex-rs/report/assets/report-contract";

const timestamp = z.string().datetime({ offset: true });
const count = z.number().int().nonnegative().safe();
const usageSchema = z
  .object({
    timestamp,
    threadId: z.string(),
    responseId: z.string().nullable(),
    accountId: z.string().nullable(),
    requestedModel: z.string().nullable(),
    requestedServiceTier: z.string().nullable(),
    reportedModel: z.string().nullable(),
    reportedServiceTier: z.string().nullable(),
    inputTokens: count,
    cachedInputTokens: count,
    cacheWriteInputTokens: count,
    outputTokens: count,
    reasoningOutputTokens: count,
    source: z.enum(["exact", "legacyApproximate"]),
  })
  .refine(
    (row) =>
      row.cachedInputTokens <= row.inputTokens &&
      row.reasoningOutputTokens <= row.outputTokens,
    {
      message: "Token subsets exceed their totals",
    },
  );

export const reportSchema = z.object({
  schemaVersion: z.literal(1),
  generatedAt: timestamp,
  usage: z.array(usageSchema),
  limits: z.array(
    z.object({
      timestamp,
      accountId: z.string(),
      limitId: z.string(),
      windowSeconds: count.nullable(),
      usedPercent: z.number().finite().nonnegative().nullable(),
      resetsAt: timestamp.nullable(),
      source: z.string(),
    }),
  ),
  resets: z.array(
    z.object({
      timestamp,
      accountId: z.string(),
      idempotencyKey: z.string().nullable(),
      limitId: z.string().nullable(),
    }),
  ),
  diagnostics: z.object({
    filesDiscovered: count,
    filesScanned: count,
    fileReadErrors: count,
    discoveryErrors: count,
    telemetryReadErrors: count,
    oversizedRecords: count,
    recordsParsed: count,
    malformedRecords: count,
    duplicateUsageRecords: count,
    conflictingUsageRecords: count,
    exactUsageRecords: count,
    legacyUsageRecords: count,
  }),
}) satisfies z.ZodType<ReportData>;

export type { LimitRecord, ReportData, ResetRecord, UsageRecord };

export function parseReport(value: unknown): ReportData {
  return reportSchema.parse(value);
}
