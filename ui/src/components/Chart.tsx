import { useEffect, useRef } from "preact/hooks";
import uPlot from "uplot";
import { clock, clockShort, formatDuration, formatValue } from "../format";
import { canvasPattern } from "../pixel/dither";
import { usePalette, usePrefs, type Zone } from "../prefs";
import "uplot/dist/uPlot.min.css";
import "./Chart.css";

export interface Band {
  /** Seconds since the epoch. */
  from: number;
  to: number;
  kind: "reference" | "abnormal";
}

interface Props {
  /** Window start times, seconds since the epoch. */
  times: number[];
  values: (number | null)[];
  height?: number;
  /** Shaded periods: the analysis's reference and abnormal windows. */
  bands?: Band[];
  /** A vertical rule, seconds since the epoch (the anomaly time). */
  marker?: number;
  label?: string;
  /** Small multiples: smaller type, no annotations. */
  compact?: boolean;
  /** The typical level (reference median), drawn as a dashed rule. */
  typical?: number;
  /** The peak, [seconds since the epoch, value], marked with a square. */
  peak?: [number, number];
  /** The part of the trace inside this period is drawn in the signal colour. */
  emphasis?: [number, number];
  /** Draw the whole trace in the signal colour. */
  alert?: boolean;
}

function css(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/**
 * A date whose local fields are the UTC fields of `ts` (seconds): uPlot
 * places time ticks in local time, and this makes it place them in UTC
 * without the Intl time zone database.
 */
function utcDate(ts: number): Date {
  const d = new Date(ts * 1e3);
  return new Date(d.getTime() + d.getTimezoneOffset() * 60e3);
}

function timeLabels(zone: Zone) {
  return (_u: uPlot, splits: number[], _axis: number, _space: number, incr: number) =>
    splits.map((t) => (incr < 60 ? clock(t * 1e9, zone) : clockShort(t * 1e9, zone)));
}

/** A time series chart in the instrument style of the UI. */
export function Chart({
  times,
  values,
  height = 150,
  bands = [],
  marker,
  label = "value",
  compact = false,
  typical,
  peak,
  emphasis,
  alert = false,
}: Props) {
  const host = useRef<HTMLDivElement>(null);
  const readout = useRef<HTMLDivElement>(null);
  const { zone } = usePrefs();
  const palette = usePalette();

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const c = {
      ink: css("--ink"),
      muted: css("--ink-3"),
      rule: css("--rule"),
      grid: css("--rule-2"),
      signal: css("--signal"),
      signalText: css("--signal-text"),
      sheet: css("--sheet"),
    };
    const px = uPlot.pxRatio || 1;
    const fontPx = compact ? 9.5 : 10;
    const font = `${Math.round(fontPx * px)}px "IBM Plex Mono", ui-monospace, monospace`;
    const axisFont = `${fontPx}px "IBM Plex Mono", ui-monospace, monospace`;
    const cell = Math.max(1, Math.round(2 * px));

    const patterns = new Map<Band["kind"], CanvasPattern | null>();
    const drawBands = (u: uPlot) => {
      const { ctx } = u;
      const { top, height: h, left, width: w } = u.bbox;
      ctx.save();
      for (const b of bands) {
        const x0 = Math.max(left, Math.round(u.valToPos(b.from, "x", true)));
        const x1 = Math.min(left + w, Math.round(u.valToPos(b.to, "x", true)));
        if (x1 <= x0) continue;
        if (!patterns.has(b.kind))
          patterns.set(b.kind, canvasPattern(ctx, b.kind === "abnormal" ? c.signal : c.muted, 2, cell));
        const pattern = patterns.get(b.kind);
        if (pattern) {
          ctx.fillStyle = pattern;
          ctx.fillRect(x0, top, x1 - x0, h);
        }
      }
      ctx.restore();
    };

    const annotate = (u: uPlot) => {
      const { ctx } = u;
      const { top, height: h, left, width: w } = u.bbox;
      ctx.save();
      ctx.font = font;
      ctx.textAlign = "left";
      ctx.textBaseline = "middle";
      ctx.lineWidth = px;
      // Frame of the plotting area.
      ctx.strokeStyle = c.rule;
      ctx.strokeRect(left + px / 2, top + px / 2, w - px, h - px);
      if (!compact) {
        // Each band's extent as a dimension line above the plot.
        const y = Math.round(top - 9 * px) + 0.5;
        for (const b of bands) {
          const x0 = Math.round(Math.max(left, u.valToPos(b.from, "x", true))) + 0.5;
          const x1 = Math.round(Math.min(left + w, u.valToPos(b.to, "x", true))) - 0.5;
          if (x1 - x0 < 6 * px) continue;
          const color = b.kind === "abnormal" ? c.signalText : c.muted;
          ctx.strokeStyle = color;
          ctx.beginPath();
          ctx.moveTo(x0, y);
          ctx.lineTo(x1, y);
          ctx.moveTo(x0, y - 4 * px);
          ctx.lineTo(x0, y + 4 * px);
          ctx.moveTo(x1, y - 4 * px);
          ctx.lineTo(x1, y + 4 * px);
          ctx.stroke();
          const name = b.kind === "abnormal" ? "ABNORMAL" : "REFERENCE";
          const full = `${name} ${formatDuration(b.to - b.from)}`;
          const text = [full, name].find((t) => ctx.measureText(t).width + 16 * px < x1 - x0);
          if (text) {
            const tw = ctx.measureText(text).width;
            const tx = Math.round(x0 + 8 * px);
            ctx.fillStyle = c.sheet;
            ctx.fillRect(tx - 4 * px, y - 6 * px, tw + 8 * px, 12 * px);
            ctx.fillStyle = color;
            ctx.fillText(text, tx, y + px);
          }
        }
      }
      if (typical !== undefined && Number.isFinite(typical)) {
        const y = Math.round(u.valToPos(typical, "y", true)) + 0.5;
        if (y >= top - 1 && y <= top + h + 1) {
          ctx.strokeStyle = c.muted;
          ctx.setLineDash([4 * px, 3 * px]);
          ctx.beginPath();
          ctx.moveTo(left, y);
          ctx.lineTo(left + w, y);
          ctx.stroke();
          ctx.setLineDash([]);
          if (!compact) {
            // Labelled in the right margin, where it hides no data.
            ctx.fillStyle = c.muted;
            ctx.fillText("typ", left + w + 5 * px, y - 6 * px);
            ctx.fillText(formatValue(typical), left + w + 5 * px, y + 6 * px);
          }
        }
      }
      if (marker !== undefined) {
        const x = Math.round(u.valToPos(marker, "x", true)) + 0.5;
        if (x >= left && x <= left + w) {
          ctx.strokeStyle = c.signal;
          ctx.beginPath();
          ctx.moveTo(x, top);
          ctx.lineTo(x, top + h);
          ctx.stroke();
        }
      }
      if (peak && !compact) {
        const x = Math.round(u.valToPos(peak[0], "x", true));
        const y = Math.round(u.valToPos(peak[1], "y", true));
        if (x >= left && x <= left + w && y >= top - 1 && y <= top + h + 1) {
          const s = Math.round(5 * px);
          ctx.fillStyle = c.sheet;
          ctx.fillRect(x - s / 2 - px, y - s / 2 - px, s + 2 * px, s + 2 * px);
          ctx.fillStyle = c.signal;
          ctx.fillRect(x - s / 2, y - s / 2, s, s);
          const text = `peak ${formatValue(peak[1])}`;
          const tw = ctx.measureText(text).width;
          const right = x + 10 * px + tw < left + w;
          const tx = right ? x + 9 * px : x - 9 * px - tw;
          const ty = Math.min(Math.max(top + 8 * px, y), top + h - 8 * px);
          ctx.fillStyle = c.sheet;
          ctx.fillRect(tx - 3 * px, ty - 6 * px, tw + 6 * px, 12 * px);
          ctx.fillStyle = c.signalText;
          ctx.fillText(text, tx, ty + px);
        }
      }
      ctx.restore();
    };

    const axis = {
      stroke: c.muted,
      font: axisFont,
      grid: { stroke: c.grid, width: 1 },
      ticks: { show: true, stroke: c.rule, width: 1, size: 3 },
    };
    const masked = emphasis && values.map((v, i) => (times[i]! >= emphasis[0] && times[i]! <= emphasis[1] ? v : null));
    const series: uPlot.Series[] = [
      {},
      { label, stroke: alert ? c.signal : c.ink, width: 1.25, points: { show: false }, spanGaps: false },
    ];
    const data: uPlot.AlignedData = masked ? [times, values, masked] : [times, values];
    if (masked) series.push({ label: `${label}, abnormal`, stroke: c.signal, width: 1.75, points: { show: false } });

    const opts: uPlot.Options = {
      width: el.clientWidth || 600,
      height,
      legend: { show: false },
      padding: compact ? [8, 8, 0, 0] : [bands.length ? 22 : 10, typical !== undefined ? 46 : 10, 0, 0],
      cursor: { y: false, points: { show: false }, drag: { x: false, y: false } },
      tzDate: zone === "utc" ? utcDate : (ts) => new Date(ts * 1e3),
      scales: {
        x: { time: true },
        y: {
          range: (_u, lo, hi) => {
            let a = Number.isFinite(lo) ? lo : 0;
            let b = Number.isFinite(hi) ? hi : 1;
            if (typical !== undefined && Number.isFinite(typical)) {
              a = Math.min(a, typical);
              b = Math.max(b, typical);
            }
            const nonNegative = a >= 0;
            if (a === b) {
              const d = Math.abs(a) * 0.1 || 1;
              a -= d;
              b += d;
            }
            const pad = (b - a) * 0.12;
            a -= pad;
            b += pad;
            // Data that cannot be negative never gets a negative axis.
            return [nonNegative ? Math.max(0, a) : a, b];
          },
        },
      },
      axes: [
        { ...axis, space: compact ? 64 : 84, size: compact ? 20 : 24, values: timeLabels(zone) },
        {
          ...axis,
          size: compact ? 42 : 52,
          space: compact ? 22 : 28,
          values: (_u: uPlot, splits: number[]) => splits.map(formatValue),
        },
      ],
      series,
      hooks: {
        drawClear: [drawBands],
        draw: [annotate],
        setCursor: [
          (u) => {
            const out = readout.current;
            if (!out) return;
            const i = u.cursor.idx;
            out.textContent =
              i === null || i === undefined ? "" : `${clock(times[i]! * 1e9, zone)}  ${formatValue(values[i])}`;
          },
        ],
      },
    };
    const plot = new uPlot(opts, data, el);
    let alive = true;
    void document.fonts?.ready.then(() => alive && plot.redraw(false, true));
    const resize = new ResizeObserver(() => plot.setSize({ width: el.clientWidth, height }));
    resize.observe(el);
    return () => {
      alive = false;
      resize.disconnect();
      plot.destroy();
    };
  }, [times, values, height, bands, marker, label, compact, typical, peak, emphasis, alert, zone, palette]);

  return (
    <div class={compact ? "chart compact" : "chart"}>
      <div ref={host} />
      <div class="chart-readout" ref={readout} aria-hidden="true" />
    </div>
  );
}
