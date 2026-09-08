import { useCallback, useEffect, useMemo, useState } from "react";
import type { ChangeEvent } from "react";
import {
  Activity,
  ArrowUpRight,
  Check,
  Command,
  Fingerprint,
  Moon,
  Sun,
  Monitor,
  ShieldCheck,
} from "lucide-react";
import {
  activity,
  breakdown,
  compact,
  dateTime,
  initialFilters,
  selectReport,
  totals,
} from "./data/aggregate";
import type { ReportData } from "./data/report-data";
import { activityOption, breakdownOption } from "./charts/chart-options";
import { ReportChart } from "./charts/report-chart";
import { FilterBar } from "./components/filter-bar";
import { Metrics } from "./components/metrics";
import { LimitPanels } from "./components/limit-panels";
import { UsageTable } from "./components/usage-table";

export type ThemePreference = "system" | "dark" | "light";
export interface DashboardProps {
  data: ReportData;
  initialTheme: ThemePreference;
}

export function Dashboard({ data, initialTheme }: DashboardProps) {
  const [filters, setFilters] = useState(() =>
    initialFilters(data.generatedAt),
  );
  const [bucket, setBucket] = useState<"day" | "hour" | "week">("day");
  const [preference, setPreference] = useState(initialTheme);
  const [systemDark, setSystemDark] = useState(
    window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  const theme =
    preference === "system" ? (systemDark ? "dark" : "light") : preference;
  const selected = useMemo(() => selectReport(data, filters), [data, filters]);
  const sum = useMemo(
    () => totals(selected.usage),
    [selected.usage, filters.attribution],
  );
  const timeline = useMemo(
    () => activityOption(activity(selected.usage, bucket)),
    [selected.usage, bucket],
  );
  const models = useMemo(
    () =>
      breakdownOption(breakdown(selected.usage, "model", filters.attribution)),
    [selected.usage, filters.attribution],
  );
  const tiers = useMemo(
    () =>
      breakdownOption(breakdown(selected.usage, "tier", filters.attribution)),
    [selected.usage, filters.attribution],
  );
  const handleBucket = useCallback((event: ChangeEvent<HTMLSelectElement>) => {
    if (
      event.currentTarget.value === "day" ||
      event.currentTarget.value === "hour" ||
      event.currentTarget.value === "week"
    )
      setBucket(event.currentTarget.value);
  }, []);
  const handleTheme = useCallback(
    () =>
      setPreference((value) =>
        value === "system" ? "dark" : value === "dark" ? "light" : "system",
      ),
    [],
  );
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.style.colorScheme = theme;
    try {
      localStorage.setItem("better-codex-report-theme", preference);
    } catch {
      /* Local files may not expose browser storage. */
    }
  }, [theme, preference]);
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const change = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  const ThemeIcon =
    preference === "system" ? Monitor : preference === "dark" ? Moon : Sun;
  const d = data.diagnostics;
  return (
    <div className="report-shell">
      <header className="topbar">
        <a className="brand" href="#overview">
          <span className="brand-mark">
            <Command size={21} />
          </span>
          better<span className="brand-light">codex</span>
          <span className="brand-divider" />
          <span className="brand-section">INSIGHTS</span>
        </a>
        <div className="topbar-actions">
          <span className="local-badge">
            <ShieldCheck size={13} /> Local report
          </span>
          <button
            className="theme-button"
            onClick={handleTheme}
            aria-label={`Theme: ${preference}. Change theme.`}
            title={`Theme: ${preference}`}
          >
            <ThemeIcon size={17} />
            <span>{preference}</span>
          </button>
        </div>
      </header>
      <main id="overview">
        <div className="hero">
          <div>
            <p className="eyebrow">
              <span className="status-dot" /> YOUR WORK, IN PERSPECTIVE
            </p>
            <h1>
              A clearer view of
              <br className="mobile-break" /> your compute<span>.</span>
            </h1>
            <p className="hero-description">
              Every model. Every token. The full picture.
            </p>
          </div>
          <div className="report-stamp">
            <Fingerprint size={18} />
            <div>
              <span>SNAPSHOT GENERATED</span>
              <time>{dateTime.format(new Date(data.generatedAt))} UTC</time>
            </div>
          </div>
        </div>
        <FilterBar data={data} filters={filters} onChange={setFilters} />
        {d.fileReadErrors +
          d.discoveryErrors +
          d.telemetryReadErrors +
          d.oversizedRecords >
          0 && (
          <p className="coverage-warning" role="status">
            Some history could not be read. {d.fileReadErrors} file errors ·{" "}
            {d.discoveryErrors} discovery errors · {d.telemetryReadErrors}{" "}
            telemetry errors · {d.oversizedRecords} oversized records. See data
            coverage below.
          </p>
        )}
        {filters.start && filters.end && filters.start > filters.end && (
          <p role="alert" className="coverage-warning">
            Choose an end date on or after the start date.
          </p>
        )}
        <Metrics totals={sum} />
        <section className="panel activity-panel">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">MOMENTUM</p>
              <h2>
                Token activity <span className="live-dot" />
              </h2>
              <p>
                Input and output, with cache reuse in view. Long ranges combine
                adjacent buckets.
              </p>
            </div>
            <label className="bucket-select">
              <span className="sr-only">Time bucket</span>
              <select
                aria-label="Time bucket"
                value={bucket}
                onChange={handleBucket}
              >
                <option value="day">Daily</option>
                <option value="hour">Hourly</option>
                <option value="week">Weekly</option>
              </select>
            </label>
          </div>
          <div className="activity-total">
            {compact.format(sum.input + sum.output)} <span>total tokens</span>
            <span className="chart-hint">
              <ArrowUpRight size={13} /> Scroll to zoom · drag to explore
            </span>
          </div>
          {selected.usage.length ? (
            <ReportChart
              option={timeline}
              zoomable
              theme={theme}
              label="Stacked token activity: uncached input, cached input, and output. Exact values are available in the response ledger."
            />
          ) : (
            <div className="empty chart-empty">
              <Activity size={28} />
              <h3>No activity in this view</h3>
              <p>Try a wider date range or clear a filter.</p>
            </div>
          )}
        </section>
        <div className="breakdown-grid">
          <section className="panel">
            <div className="panel-heading">
              <div>
                <p className="eyebrow">MODEL MIX</p>
                <h2>Where the tokens went</h2>
              </div>
              <span className="panel-tag">Input + output</span>
            </div>
            {selected.usage.length ? (
              <ReportChart
                option={models}
                theme={theme}
                label="Top eight models by input plus output tokens. Full values are in the response ledger."
                className="breakdown-chart"
              />
            ) : (
              <p className="empty compact-empty">No model usage recorded.</p>
            )}
          </section>
          <section className="panel">
            <div className="panel-heading">
              <div>
                <p className="eyebrow">SERVICE TIERS</p>
                <h2>The pace of your work</h2>
              </div>
              <span className="panel-tag">Requested or confirmed</span>
            </div>
            {selected.usage.length ? (
              <ReportChart
                option={tiers}
                theme={theme}
                label="Top eight recorded service tiers by input plus output tokens."
                className="breakdown-chart"
              />
            ) : (
              <p className="empty compact-empty">
                No service-tier usage recorded.
              </p>
            )}
          </section>
        </div>
        <LimitPanels
          limits={selected.limits}
          resets={selected.resets}
          theme={theme}
        />
        <UsageTable rows={selected.usage} attribution={filters.attribution} />
        <details className="coverage">
          <summary>
            <Check size={15} /> Data coverage{" "}
            <span>
              {d.exactUsageRecords} exact · {d.legacyUsageRecords} approximate
            </span>
          </summary>
          <div className="coverage-content">
            <p>
              This report was generated from {d.filesScanned} of{" "}
              {d.filesDiscovered} discovered session files and {d.recordsParsed}{" "}
              records. Legacy usage is reconstructed approximately; its entries
              do not necessarily correspond one-to-one with model requests.
              Unknown accounts, models, and tiers remain unattributed.
            </p>
            <p>
              {d.malformedRecords} malformed records skipped ·{" "}
              {d.duplicateUsageRecords} duplicates removed ·{" "}
              {d.conflictingUsageRecords} conflicting records. Cached input and
              reasoning output are subsets of input and output. All displayed
              dates use UTC.
            </p>
          </div>
        </details>
        <footer>
          <span>
            <Command size={13} /> better codex / insights
          </span>
          <span>Made from your sessions. Stays on your machine.</span>
        </footer>
      </main>
    </div>
  );
}
