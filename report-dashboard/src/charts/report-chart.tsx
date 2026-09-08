import { useCallback, useEffect, useRef } from "react";
import { RotateCcw } from "lucide-react";
import { init, use } from "echarts/core";
import { BarChart, LineChart } from "echarts/charts";
import {
  AriaComponent,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  MarkLineComponent,
  TooltipComponent,
} from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import type { EChartsOption } from "echarts";

use([
  BarChart,
  LineChart,
  AriaComponent,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  MarkLineComponent,
  TooltipComponent,
  CanvasRenderer,
]);

export interface ReportChartProps {
  option: EChartsOption;
  theme: "light" | "dark";
  label: string;
  className?: string;
  zoomable?: boolean;
  onAnnotationSelect?: (timestamp: string) => void;
}

export function ReportChart({
  option,
  theme,
  label,
  className = "",
  zoomable = false,
  onAnnotationSelect,
}: ReportChartProps) {
  const container = useRef<HTMLDivElement>(null);
  const instance = useRef<ReturnType<typeof init> | null>(null);
  const handleResetZoom = useCallback(
    () =>
      instance.current?.dispatchAction({
        type: "dataZoom",
        start: 0,
        end: 100,
      }),
    [],
  );
  useEffect(() => {
    if (!container.current) return;
    const chart = init(container.current, undefined, { renderer: "canvas" });
    instance.current = chart;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    chart.setOption({
      ...option,
      useUTC: true,
      animation: !reduced,
      animationDuration: 250,
      backgroundColor: "transparent",
      textStyle: {
        color: theme === "dark" ? "#a2b0c3" : "#526274",
        fontFamily: "system-ui, sans-serif",
      },
      aria: { enabled: true, decal: { show: false } },
    });
    chart.on("click", (event) => {
      if (event.componentType === "markLine" && onAnnotationSelect)
        onAnnotationSelect(event.name);
    });
    const observer = new ResizeObserver(() => chart.resize());
    observer.observe(container.current);
    return () => {
      observer.disconnect();
      chart.dispose();
      instance.current = null;
    };
  }, [option, theme, onAnnotationSelect]);
  return (
    <div className="chart-container">
      <div
        ref={container}
        className={`chart ${className}`}
        role="img"
        aria-label={label}
      />
      {zoomable && (
        <button className="text-button zoom-reset" onClick={handleResetZoom}>
          <RotateCcw size={11} /> Reset zoom
        </button>
      )}
    </div>
  );
}
