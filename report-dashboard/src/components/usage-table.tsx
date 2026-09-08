import { useCallback, useMemo, useState } from "react";
import { ArrowDownWideNarrow, ChevronLeft, ChevronRight } from "lucide-react";
import { attributionValue, dateTime, integer } from "../data/aggregate";
import type { Filters } from "../data/aggregate";
import type { UsageRecord } from "../data/report-data";

export interface UsageTableProps {
  /** Ascending timestamp order from codex-report; filtering preserves that order. */
  rows: UsageRecord[];
  attribution: Filters["attribution"];
}

export function UsageTable({ rows, attribution }: UsageTableProps) {
  const [sort, setSort] = useState<"date" | "tokens">("date");
  const [page, setPage] = useState(0);
  const sorted = useMemo(
    () =>
      sort === "date"
        ? [...rows].reverse()
        : [...rows].sort(
            (a, b) =>
              b.inputTokens + b.outputTokens - a.inputTokens - a.outputTokens,
          ),
    [rows, sort],
  );
  const lastPage = Math.max(0, Math.ceil(rows.length / 25) - 1);
  const current = Math.min(page, lastPage);
  const handleSort = useCallback(() => {
    setSort((value) => (value === "date" ? "tokens" : "date"));
    setPage(0);
  }, []);
  const handlePrevious = useCallback(
    () => setPage(Math.max(0, current - 1)),
    [current],
  );
  const handleNext = useCallback(
    () => setPage(Math.min(lastPage, current + 1)),
    [current, lastPage],
  );
  return (
    <section className="panel usage-panel">
      <div className="panel-heading">
        <div>
          <p className="eyebrow">THE DETAIL</p>
          <h2>
            Response ledger{" "}
            <span className="count">{integer.format(rows.length)}</span>
          </h2>
        </div>
        <button className="subtle-button" onClick={handleSort}>
          <ArrowDownWideNarrow size={15} />{" "}
          {sort === "date" ? "Newest first" : "Most tokens"}
        </button>
      </div>
      <div className="table-scroll">
        <table>
          <caption className="sr-only">
            Individual recorded usage; timestamps in UTC. Cache and reasoning
            columns are subsets.
          </caption>
          <thead>
            <tr>
              <th>Time · UTC</th>
              <th>Model / account</th>
              <th>Tier</th>
              <th className="numeric">Input</th>
              <th className="numeric">Cached</th>
              <th className="numeric">Output</th>
              <th className="numeric">Reasoning</th>
              <th>Evidence</th>
            </tr>
          </thead>
          <tbody>
            {sorted.slice(current * 25, current * 25 + 25).map((row, index) => (
              <tr
                key={`${row.threadId}-${row.responseId}-${row.timestamp}-${index}`}
              >
                <td className="nowrap">
                  {dateTime.format(new Date(row.timestamp))}
                </td>
                <td>
                  <span
                    className="table-model"
                    title={
                      attributionValue(row, "model", attribution) ?? "Unknown"
                    }
                  >
                    {attributionValue(row, "model", attribution) ?? "Unknown"}
                  </span>
                  <span
                    className="table-account"
                    title={row.accountId ?? "Unknown account"}
                  >
                    {row.accountId ?? "Unknown account"}
                  </span>
                </td>
                <td>
                  <span className="tier-tag">
                    {attributionValue(row, "tier", attribution) ?? "Unknown"}
                  </span>
                </td>
                <td className="numeric">{integer.format(row.inputTokens)}</td>
                <td className="numeric cached-text">
                  {integer.format(row.cachedInputTokens)}
                </td>
                <td className="numeric">{integer.format(row.outputTokens)}</td>
                <td className="numeric">
                  {integer.format(row.reasoningOutputTokens)}
                </td>
                <td>
                  <span
                    className={
                      row.source === "exact"
                        ? "evidence"
                        : "evidence approximate"
                    }
                  >
                    {row.source === "exact" ? "Exact" : "Approximate"}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {rows.length === 0 && (
          <p className="empty compact-empty">
            No responses match these filters.
          </p>
        )}
      </div>
      <div className="table-footer">
        <span>
          {rows.length
            ? `${current * 25 + 1}–${Math.min((current + 1) * 25, rows.length)} of ${integer.format(rows.length)}`
            : "0 responses"}
        </span>
        <div>
          <button
            className="icon-button"
            aria-label="Previous page"
            onClick={handlePrevious}
            disabled={current === 0}
          >
            <ChevronLeft size={16} />
          </button>
          <button
            className="icon-button"
            aria-label="Next page"
            onClick={handleNext}
            disabled={current === lastPage}
          >
            <ChevronRight size={16} />
          </button>
        </div>
      </div>
    </section>
  );
}
