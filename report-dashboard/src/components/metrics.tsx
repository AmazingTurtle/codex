import {
  ArrowDownLeft,
  ArrowUpRight,
  DatabaseZap,
  Layers3,
  Zap,
} from "lucide-react";
import { cachedInputShare, compact, integer } from "../data/aggregate";
import type { TokenTotals } from "../data/aggregate";

export interface MetricsProps {
  totals: TokenTotals;
}

export function Metrics({ totals }: MetricsProps) {
  const share = cachedInputShare(totals);
  const cards = [
    {
      label: "Responses",
      value: compact.format(totals.responses),
      detail: "Recorded model responses",
      icon: Layers3,
      raw: totals.responses,
    },
    {
      label: "Input tokens",
      value: compact.format(totals.input),
      detail: "Includes cached input",
      icon: ArrowDownLeft,
      raw: totals.input,
    },
    {
      label: "Output tokens",
      value: compact.format(totals.output),
      detail: `${compact.format(totals.reasoning)} reasoning tokens included`,
      icon: ArrowUpRight,
      raw: totals.output,
    },
    {
      label: "Cached input",
      value: compact.format(totals.cached),
      detail: `${compact.format(totals.cacheWrites)} cache-write tokens recorded`,
      icon: DatabaseZap,
      raw: totals.cached,
    },
    {
      label: "Cached-input share",
      value: share === null ? "—" : `${share.toFixed(1)}%`,
      detail: "Cached ÷ total input tokens",
      icon: Zap,
      raw: null,
    },
  ];
  return (
    <section className="metrics" aria-label="Usage summary">
      {cards.map(({ label, value, detail, icon: Icon, raw }) => (
        <article className="metric" key={label}>
          <div className="metric-label">
            <span>{label}</span>
            <Icon size={16} aria-hidden="true" />
          </div>
          <div
            className="metric-value"
            title={raw === null ? detail : integer.format(raw)}
          >
            {value}
          </div>
          <div className="metric-detail">{detail}</div>
        </article>
      ))}
    </section>
  );
}
