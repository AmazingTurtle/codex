import type { EChartsOption, LineSeriesOption } from "echarts";
import type { ActivityBucket } from "../data/aggregate";
import type { LimitRecord, ResetRecord } from "../data/report-data";

const colors = [
  "#39c8b4",
  "#8b8cf6",
  "#f3b65c",
  "#55a5ec",
  "#ec82b3",
  "#b4cb6f",
];
const axis = {
  axisLine: { show: false },
  axisTick: { show: false },
  axisLabel: { color: "#8996a8", fontSize: 11 },
  splitLine: { lineStyle: { color: "#8090a020", type: "dashed" as const } },
};
const tooltip = {
  trigger: "axis" as const,
  renderMode: "richText" as const,
  confine: true,
  backgroundColor: "#172131",
  borderColor: "#344258",
  textStyle: { color: "#eef3fb", fontSize: 12 },
};

export function activityOption(buckets: ActivityBucket[]): EChartsOption {
  return {
    color: colors,
    tooltip,
    grid: { top: 24, left: 12, right: 16, bottom: 68, containLabel: true },
    legend: {
      bottom: 0,
      icon: "circle",
      textStyle: { color: "#8996a8" },
      itemWidth: 8,
      itemHeight: 8,
    },
    xAxis: { type: "time", ...axis, splitLine: { show: false } },
    yAxis: {
      type: "value",
      ...axis,
      axisLabel: {
        color: "#8996a8",
        formatter: (value: number) =>
          Intl.NumberFormat("en", { notation: "compact" }).format(value),
      },
    },
    dataZoom: [
      { type: "inside", filterMode: "none" },
      {
        type: "slider",
        height: 16,
        bottom: 30,
        borderColor: "transparent",
        fillerColor: "#39c8b420",
        showDetail: false,
        brushSelect: false,
      },
    ],
    series: [
      {
        name: "Uncached input",
        pick: (row: ActivityBucket) => row.input - row.cached,
      },
      { name: "Cached input", pick: (row: ActivityBucket) => row.cached },
      { name: "Output", pick: (row: ActivityBucket) => row.output },
    ].map(({ name, pick }) => ({
      name,
      type: "bar",
      stack: "tokens",
      barMaxWidth: 30,
      emphasis: { focus: "series" },
      itemStyle: { borderRadius: [3, 3, 0, 0] },
      data: buckets.map((row) => [row.timestamp, pick(row)]),
    })),
  };
}

export function breakdownOption(
  items: { name: string; value: number }[],
): EChartsOption {
  const top = items.slice(0, 8);
  return {
    tooltip: { ...tooltip, trigger: "item" },
    grid: { top: 10, left: 0, right: 52, bottom: 0, containLabel: true },
    xAxis: { show: false, type: "value" },
    yAxis: {
      ...axis,
      type: "category",
      inverse: true,
      data: top.map((item) => item.name),
      axisLabel: {
        color: "#8996a8",
        width: 105,
        overflow: "truncate",
        fontSize: 11,
      },
    },
    series: [
      {
        type: "bar",
        barMaxWidth: 13,
        data: top.map((item, index) => ({
          value: item.value,
          itemStyle: {
            color: colors[index % colors.length] ?? "#39c8b4",
            borderRadius: 4,
          },
        })),
        label: {
          show: true,
          position: "right",
          color: "#8996a8",
          formatter: (params) =>
            typeof params.value === "number"
              ? Intl.NumberFormat("en", { notation: "compact" }).format(
                  params.value,
                )
              : "",
        },
      },
    ],
  };
}

export function limitOption(
  rows: LimitRecord[],
  resets: ResetRecord[],
): EChartsOption {
  const windows = new Map<string, LimitRecord[]>();
  for (const row of rows) {
    const name = `${row.limitId} · ${row.windowSeconds === null ? "unknown window" : row.windowSeconds >= 86400 ? `${Math.round(row.windowSeconds / 86400)}d` : `${Math.round(row.windowSeconds / 3600)}h`}`;
    const group = windows.get(name);
    if (group) group.push(row);
    else windows.set(name, [row]);
  }
  const series: LineSeriesOption[] = [...windows].map(([name, group]) => {
    const points: [number, number | null][] = [];
    let previous: number | undefined;
    for (const row of [...group].sort(
      (a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp),
    )) {
      const time = Date.parse(row.timestamp);
      if (previous !== undefined && time - previous > 900_000)
        points.push([previous + 1, null]);
      points.push([time, row.usedPercent]);
      previous = time;
    }
    return {
      name,
      type: "line",
      step: "end",
      showSymbol: true,
      symbolSize: 4,
      connectNulls: false,
      data: points,
      lineStyle: { width: 2 },
      emphasis: { focus: "series" },
    };
  });
  // An annotation-only series also shows resets when no observations survived the filter.
  series.push({
    name: "Banked reset",
    type: "line",
    data: resets.map((row) => [Date.parse(row.timestamp), null]),
    markLine: {
      silent: false,
      symbol: ["none", "none"],
      label: { show: false },
      lineStyle: { color: "#f3b65c", type: "dashed", width: 2 },
      data: resets.map((row) => ({
        name: row.timestamp,
        xAxis: Date.parse(row.timestamp),
      })),
    },
  });
  return {
    color: colors,
    tooltip,
    grid: { top: 15, left: 0, right: 12, bottom: 36, containLabel: true },
    legend: {
      bottom: 0,
      type: "scroll",
      textStyle: { color: "#8996a8", fontSize: 10 },
      icon: "roundRect",
      itemWidth: 10,
      itemHeight: 3,
    },
    xAxis: { ...axis, type: "time", splitLine: { show: false } },
    yAxis: {
      ...axis,
      type: "value",
      min: 0,
      max: 100,
      axisLabel: { color: "#8996a8", formatter: "{value}%" },
    },
    series,
  };
}
