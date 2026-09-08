// GENERATED CODE! DO NOT MODIFY BY HAND!

export type UsageSource = "exact" | "legacyApproximate";

export type UsageRecord = { timestamp: string, threadId: string, responseId: string | null, accountId: string | null, requestedModel: string | null, requestedServiceTier: string | null, reportedModel: string | null, reportedServiceTier: string | null, inputTokens: number, cachedInputTokens: number, cacheWriteInputTokens: number, outputTokens: number, reasoningOutputTokens: number, source: UsageSource, };

export type LimitRecord = { timestamp: string, accountId: string, limitId: string, windowSeconds: number | null, usedPercent: number | null, resetsAt: string | null, source: string, };

export type ResetRecord = { timestamp: string, accountId: string, idempotencyKey: string | null, limitId: string | null, };

export type Diagnostics = { filesDiscovered: number, filesScanned: number, fileReadErrors: number, discoveryErrors: number, telemetryReadErrors: number, oversizedRecords: number, recordsParsed: number, malformedRecords: number, duplicateUsageRecords: number, conflictingUsageRecords: number, exactUsageRecords: number, legacyUsageRecords: number, };

export type ReportData = { schemaVersion: number, generatedAt: string, usage: Array<UsageRecord>, limits: Array<LimitRecord>, resets: Array<ResetRecord>, diagnostics: Diagnostics, };
