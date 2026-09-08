import { useCallback, useMemo, useState } from "react";
import type { MouseEvent } from "react";
import { BatteryMedium, RotateCcw } from "lucide-react";
import { ReportChart } from "../charts/report-chart";
import { limitOption } from "../charts/chart-options";
import { dateTime } from "../data/aggregate";
import type { LimitRecord, ResetRecord } from "../data/report-data";

export interface LimitPanelsProps {
  limits: LimitRecord[];
  resets: ResetRecord[];
  theme: "light" | "dark";
}
interface AccountLimitProps extends LimitPanelsProps {
  account: string;
}

function AccountLimit({ account, limits, resets, theme }: AccountLimitProps) {
  const [pinned, setPinned] = useState<string | null>(null);
  const handleAnnotation = useCallback(
    (timestamp: string) => {
      if (resets.some((row) => row.timestamp === timestamp))
        setPinned(timestamp);
    },
    [resets],
  );
  const handleResetClick = useCallback(
    (event: MouseEvent<HTMLButtonElement>) => {
      const timestamp = event.currentTarget.dataset.timestamp;
      if (timestamp) setPinned(timestamp);
    },
    [],
  );
  const option = useMemo(() => limitOption(limits, resets), [limits, resets]);
  const latest = [...limits].reverse().find((row) => row.usedPercent !== null);
  return (
    <article className="panel limit-panel">
      <div className="panel-heading">
        <div className="account-heading">
          <span className="account-avatar">
            {account.slice(0, 2).toUpperCase()}
          </span>
          <div>
            <h3 title={account}>{account}</h3>
            <p>
              {limits.length} observations · {resets.length} banked resets
            </p>
          </div>
        </div>
        {latest && (
          <span className="limit-value">
            {latest.usedPercent?.toFixed(1)}
            <small>{latest.limitId} · % used</small>
          </span>
        )}
      </div>
      <ReportChart
        option={option}
        onAnnotationSelect={handleAnnotation}
        theme={theme}
        label={`Limit usage percentage over time for ${account}. Observations and resets follow below.`}
        className="limit-chart"
      />
      {resets.length > 0 && (
        <details className="reset-details" open={pinned !== null}>
          <summary>
            <RotateCcw size={13} /> Banked reset annotations{" "}
            <span>{resets.length}</span>
          </summary>
          <ul>
            {resets.map((reset, index) => (
              <li key={`${reset.timestamp}-${index}`}>
                <button
                  className="text-button"
                  data-timestamp={reset.timestamp}
                  onClick={handleResetClick}
                  aria-pressed={pinned === reset.timestamp}
                >
                  {dateTime.format(new Date(reset.timestamp))} UTC
                </button>
                <span>{reset.limitId ?? "Account reset"}</span>
              </li>
            ))}
          </ul>
          {pinned && (
            <p role="status">
              Pinned banked reset: {dateTime.format(new Date(pinned))} UTC
            </p>
          )}
        </details>
      )}
      <details className="observations">
        <summary>Explore observations</summary>
        <div className="table-scroll">
          <table>
            <caption className="sr-only">Account limit observations</caption>
            <thead>
              <tr>
                <th>Time · UTC</th>
                <th>Window</th>
                <th>Used</th>
                <th>Resets · UTC</th>
              </tr>
            </thead>
            <tbody>
              {limits.slice(-100).map((row, index) => (
                <tr key={`${row.timestamp}-${row.limitId}-${index}`}>
                  <td>{dateTime.format(new Date(row.timestamp))}</td>
                  <td>
                    {row.limitId} ·{" "}
                    {row.windowSeconds === null
                      ? "unknown"
                      : `${row.windowSeconds / 3600}h`}
                  </td>
                  <td>
                    {row.usedPercent === null
                      ? "Unknown"
                      : `${row.usedPercent}%`}
                  </td>
                  <td>
                    {row.resetsAt === null
                      ? "Unknown"
                      : dateTime.format(new Date(row.resetsAt))}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {limits.length > 100 && <p>Showing the latest 100 observations.</p>}
      </details>
    </article>
  );
}

export function LimitPanels({ limits, resets, theme }: LimitPanelsProps) {
  const accounts = [
    ...new Set([
      ...limits.map((row) => row.accountId),
      ...resets.map((row) => row.accountId),
    ]),
  ].sort();
  return (
    <section className="limits-section">
      <div className="section-heading">
        <div>
          <p className="eyebrow">CAPACITY</p>
          <h2>Account limits</h2>
        </div>
        <span className="section-note">
          <span className="annotation-line" /> Banked reset
        </span>
      </div>
      <p className="section-description">
        Account-wide observations, independent of model and tier filters. Gaps
        longer than 15 minutes remain open.
      </p>
      {accounts.length ? (
        <div className="limit-grid">
          {accounts.map((account) => (
            <AccountLimit
              key={account}
              account={account}
              limits={limits.filter((row) => row.accountId === account)}
              resets={resets.filter((row) => row.accountId === account)}
              theme={theme}
            />
          ))}
        </div>
      ) : (
        <div className="panel empty">
          <BatteryMedium size={28} />
          <h3>No limit history in this period</h3>
          <p>
            Limit observations and banked resets appear as they are recorded.
            Historical sessions may not contain this data.
          </p>
        </div>
      )}
    </section>
  );
}
