import { useCallback, useMemo } from "react";
import type { ChangeEvent } from "react";
import { CalendarDays, RotateCcw, SlidersHorizontal } from "lucide-react";
import { attributionValue, initialFilters, options } from "../data/aggregate";
import type { Filters } from "../data/aggregate";
import type { ReportData } from "../data/report-data";

export interface FilterBarProps {
  data: ReportData;
  filters: Filters;
  onChange: (filters: Filters) => void;
}

export function FilterBar({ data, filters, onChange }: FilterBarProps) {
  const accountOptions = useMemo(
    () =>
      options([
        ...data.usage.map((row) => row.accountId),
        ...data.limits.map((row) => row.accountId),
        ...data.resets.map((row) => row.accountId),
      ]),
    [data],
  );
  const modelOptions = useMemo(
    () =>
      options(
        data.usage.map((row) =>
          attributionValue(row, "model", filters.attribution),
        ),
      ),
    [data.usage, filters.attribution],
  );
  const tierOptions = useMemo(
    () =>
      options(
        data.usage.map((row) =>
          attributionValue(row, "tier", filters.attribution),
        ),
      ),
    [data.usage, filters.attribution],
  );
  const handleChange = useCallback(
    (event: ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
      const { name, value } = event.currentTarget;
      if (
        name === "start" ||
        name === "end" ||
        name === "account" ||
        name === "model" ||
        name === "tier"
      )
        onChange({ ...filters, [name]: value });
      if (
        name === "attribution" &&
        (value === "requested" || value === "reported")
      )
        onChange({ ...filters, attribution: value, model: "", tier: "" });
    },
    [filters, onChange],
  );
  const handleReset = useCallback(
    () => onChange(initialFilters(data.generatedAt)),
    [data.generatedAt, onChange],
  );
  const handleAllHistory = useCallback(
    () => onChange({ ...filters, start: "", end: "" }),
    [filters, onChange],
  );
  return (
    <section className="filter-bar" aria-label="Report filters">
      <div className="date-filters">
        <CalendarDays size={16} aria-hidden="true" />
        <label>
          <span className="sr-only">Start date</span>
          <input
            type="date"
            name="start"
            value={filters.start}
            onChange={handleChange}
            max={filters.end}
          />
        </label>
        <span className="muted">—</span>
        <label>
          <span className="sr-only">End date</span>
          <input
            type="date"
            name="end"
            value={filters.end}
            onChange={handleChange}
            min={filters.start}
          />
        </label>
      </div>
      <button className="text-button" onClick={handleAllHistory}>
        All history
      </button>
      <details className="filter-disclosure" open>
        <summary>
          <SlidersHorizontal size={15} /> Filters
        </summary>
        <div className="select-filters">
          <label>
            <span className="sr-only">Account</span>
            <select
              name="account"
              aria-label="Account"
              value={filters.account}
              onChange={handleChange}
            >
              <option value="">All accounts</option>
              {accountOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span className="sr-only">Model</span>
            <select
              name="model"
              aria-label="Model"
              value={filters.model}
              onChange={handleChange}
            >
              <option value="">All models</option>
              {modelOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span className="sr-only">Service tier</span>
            <select
              name="tier"
              aria-label="Service tier"
              value={filters.tier}
              onChange={handleChange}
            >
              <option value="">All tiers</option>
              {tierOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span className="sr-only">Attribution</span>
            <select
              name="attribution"
              aria-label="Attribution"
              value={filters.attribution}
              onChange={handleChange}
            >
              <option value="reported">Confirmed only</option>
              <option value="requested">Requested only</option>
            </select>
          </label>
        </div>
      </details>
      <button
        className="icon-button"
        aria-label="Reset filters"
        title="Reset filters"
        onClick={handleReset}
      >
        <RotateCcw size={16} />
      </button>
    </section>
  );
}
