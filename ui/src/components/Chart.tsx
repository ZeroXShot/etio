import { useEffect, useRef } from "preact/hooks";
import uPlot from "uplot";
import { formatValue } from "../format";
import "uplot/dist/uPlot.min.css";

export interface Band {
  from: number;
  to: number;
  kind: "reference" | "abnormal";
}

interface Props {
  /** Seconds since the epoch. */
  times: number[];
  values: (number | null)[];
  height?: number;
  bands?: Band[];
  /** A vertical marker, seconds since the epoch. */
  marker?: number;
  label?: string;
  compact?: boolean;
}

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** A time series chart with optional shaded periods. */
export function Chart({ times, values, height = 160, bands = [], marker, label = "value", compact = false }: Props) {
  const el = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = el.current;
    if (!host) return;
    const draw = (u: uPlot) => {
      const ctx = u.ctx;
      const top = u.bbox.top;
      const h = u.bbox.height;
      ctx.save();
      for (const b of bands) {
        const x0 = u.valToPos(b.from, "x", true);
        const x1 = u.valToPos(b.to, "x", true);
        ctx.fillStyle = cssVar(b.kind === "abnormal" ? "--band-abnormal" : "--band-reference");
        ctx.fillRect(x0, top, Math.max(1, x1 - x0), h);
      }
      if (marker !== undefined) {
        const x = u.valToPos(marker, "x", true);
        ctx.strokeStyle = cssVar("--danger");
        ctx.setLineDash([4, 4]);
        ctx.beginPath();
        ctx.moveTo(x, top);
        ctx.lineTo(x, top + h);
        ctx.stroke();
      }
      ctx.restore();
    };
    const axis = { stroke: cssVar("--muted"), grid: { stroke: cssVar("--grid"), width: 1 }, ticks: { show: false } };
    const opts: uPlot.Options = {
      width: host.clientWidth || 600,
      height,
      legend: { show: false },
      cursor: { drag: { x: !compact, y: false } },
      scales: { x: { time: true } },
      axes: compact
        ? [{ show: false }, { show: false }]
        : [axis, { ...axis, size: 56, values: (_u: uPlot, splits: number[]) => splits.map(formatValue) }],
      series: [{}, { label, stroke: cssVar("--accent"), width: 1.5, spanGaps: false, points: { show: false } }],
      hooks: { drawClear: [draw] },
    };
    const plot = new uPlot(opts, [times, values as (number | null)[]] as uPlot.AlignedData, host);
    const resize = new ResizeObserver(() => plot.setSize({ width: host.clientWidth, height }));
    resize.observe(host);
    return () => {
      resize.disconnect();
      plot.destroy();
    };
  }, [times, values, height, bands, marker, label, compact]);

  return <div class="chart" ref={el} />;
}
