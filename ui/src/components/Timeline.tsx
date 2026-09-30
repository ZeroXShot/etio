import type { Incident, IncidentSummary } from "../api";
import { clock, clockShort, formatDuration, offset } from "../format";
import { useWidth } from "../hooks";
import { usePrefs, type Zone } from "../prefs";
import { labelRows, lanes, scale, ticks } from "../timescale";
import { ditherFill } from "./Dither";
import "./Timeline.css";

const CHAR_W = 6.3; // advance of the 10.5 px mono face
const S = 1e9;

function zoneOffset(zone: Zone, t: number): number {
  return zone === "utc" ? 0 : -new Date(t * 1e3).getTimezoneOffset() * 60;
}

function Axis({
  from,
  to,
  x,
  y,
  width,
  zone,
}: {
  from: number;
  to: number;
  x: (t: number) => number;
  y: number;
  width: number;
  zone: Zone;
}) {
  const ts = ticks(from, to, Math.max(2, Math.floor(width / 96)), zoneOffset(zone, to)).filter(
    (t) => x(t) > 18 && x(t) < width - 44,
  );
  return (
    <g class="tl-axis">
      <line x1={0} x2={width} y1={y} y2={y} />
      {ts.map((t) => (
        <g key={t} transform={`translate(${Math.round(x(t)) + 0.5} 0)`}>
          <line class="tl-grid" y1={0} y2={y} />
          <line y1={y} y2={y + 4} />
          <text y={y + 15}>{clockShort(t * S, zone)}</text>
        </g>
      ))}
      <text class="tl-now" x={width} y={y + 15}>
        NOW
      </text>
    </g>
  );
}

/** Incidents of the retained history as bars on a time axis, one lane per overlap. */
export function IncidentHistory({
  incidents,
  now,
  hovered,
  onHover,
}: {
  incidents: IncidentSummary[];
  now: number;
  hovered: string | null;
  onHover: (id: string | null) => void;
}) {
  const { zone } = usePrefs();
  const [ref, width] = useWidth<HTMLDivElement>();
  const to = now / S;
  const earliest = Math.min(to, ...incidents.map((i) => i.start / S));
  const span = Math.max(2 * 3600, to - earliest) * 1.04;
  const from = to - span;
  const x = scale(from, to, width);
  const intervals = incidents.map((i) => [i.start / S, (i.resolved_at ?? now) / S] as const);
  const lane = lanes(intervals, span * 0.006);
  const nLanes = Math.max(1, ...lane.map((l) => l + 1));
  const BAR = 10;
  const top = 10;
  const axisY = top + nLanes * (BAR + 6) + 6;
  return (
    <div class="timeline" ref={ref}>
      <svg width={width} height={axisY + 22} role="img" aria-label="Incident history">
        <Axis from={from} to={to} x={x} y={axisY} width={width} zone={zone} />
        <line class="tl-nowline" x1={width - 0.5} x2={width - 0.5} y1={0} y2={axisY} />
        {incidents.map((inc, k) => {
          const [a, b] = intervals[k]!;
          const x0 = Math.round(x(a));
          const w = Math.max(3, Math.round(x(b)) - x0);
          const y = top + lane[k]! * (BAR + 6);
          return (
            <a
              key={inc.id}
              href={`#/incidents/${encodeURIComponent(inc.id)}`}
              class={hovered === inc.id ? "tl-bar hover" : "tl-bar"}
              onMouseEnter={() => onHover(inc.id)}
              onMouseLeave={() => onHover(null)}
              onFocus={() => onHover(inc.id)}
              onBlur={() => onHover(null)}
            >
              <title>{`${inc.id}: ${inc.status}, ${formatDuration(b - a)}`}</title>
              <rect
                x={x0}
                y={y}
                width={w}
                height={BAR}
                fill={inc.status === "open" ? ditherFill("signal", 16) : ditherFill("muted", 12)}
              />
              <rect class="tl-outline" x={x0 - 0.5} y={y - 0.5} width={w + 1} height={BAR + 1} />
            </a>
          );
        })}
      </svg>
    </div>
  );
}

interface Mark {
  t: number;
  text: string;
  tone: "signal" | "ink";
}

/**
 * How an incident relates to its analysis: the reference and abnormal
 * windows the last analysis compared, and the incident's milestones.
 */
export function IncidentWindows({ incident, now }: { incident: Incident; now: number }) {
  const { zone } = usePrefs();
  const [ref, width] = useWidth<HTMLDivElement>();
  const rca = incident.rca;
  const end = (incident.resolved_at ?? now) / S;
  const start = incident.start / S;
  const lo = rca ? Math.min(rca.reference[0], start) : start - 1200;
  const hi = Math.max(end, rca ? rca.abnormal[1] : end);
  const pad = (hi - lo) * 0.02;
  const from = lo - pad;
  const to = hi + pad;
  const x = scale(from, to, width);

  const marks: Mark[] = [
    { t: start, text: `onset ${clock(incident.start, zone)}`, tone: "signal" },
    {
      t: incident.opened_at / S,
      text: `detected ${offset((incident.opened_at - incident.start) / S)}`,
      tone: "signal",
    },
  ];
  if (incident.resolved_at !== null)
    marks.push({ t: end, text: `resolved ${clock(incident.resolved_at, zone)}`, tone: "ink" });
  else if (now - incident.last_anomaly_at > 30 * S)
    marks.push({
      t: incident.last_anomaly_at / S,
      text: `last anomaly ${clock(incident.last_anomaly_at, zone)}`,
      tone: "ink",
    });

  const extents = marks.map((m) => {
    const w = m.text.length * CHAR_W;
    const mx = x(m.t);
    return mx + w + 4 > width ? [mx - w - 4, w] : [mx + 4, w];
  });
  const rows = labelRows(
    extents.map((e) => e[0]!),
    extents.map((e) => e[1]!),
    10,
  );
  const BAND = 26;
  const labelTop = BAND + 16;
  const nRows = Math.max(1, ...rows.map((r) => r + 1));
  const axisY = labelTop + nRows * 15 + 6;

  const band = (a: number, b: number, kind: "reference" | "abnormal") => {
    const x0 = Math.round(x(a));
    const w = Math.max(1, Math.round(x(b)) - x0);
    const name = kind === "reference" ? "REFERENCE" : "ABNORMAL";
    const text = `${name} ${formatDuration(b - a)}`;
    return (
      <g class={`tl-band ${kind}`}>
        <rect x={x0} y={0} width={w} height={BAND} fill={ditherFill(kind === "abnormal" ? "signal" : "muted", 4)} />
        <rect class="tl-band-edge" x={x0 + 0.5} y={0.5} width={w - 1} height={BAND - 1} />
        {w > text.length * CHAR_W + 12 && (
          <>
            <rect class="tl-band-plate" x={x0 + 4} y={7} width={text.length * CHAR_W + 6} height={13} />
            <text x={x0 + 7} y={17}>
              {text}
            </text>
          </>
        )}
      </g>
    );
  };

  return (
    <div class="timeline windows" ref={ref}>
      <svg width={width} height={axisY + 22} role="img" aria-label="Analysis windows and incident milestones">
        <Axis from={from} to={to} x={x} y={axisY} width={width} zone={zone} />
        {rca && band(rca.reference[0], rca.reference[1], "reference")}
        {rca && band(rca.abnormal[0], rca.abnormal[1], "abnormal")}
        {!rca && (
          <rect class="tl-band-edge" x={x(start)} y={0.5} width={Math.max(1, x(end) - x(start))} height={BAND - 1} />
        )}
        <line class="tl-nowline" x1={width - 0.5} x2={width - 0.5} y1={0} y2={axisY} />
        {marks.map((m, i) => {
          const mx = Math.round(x(m.t)) + 0.5;
          const y = labelTop + rows[i]! * 15;
          return (
            <g key={m.text} class={`tl-mark ${m.tone}`}>
              <line x1={mx} x2={mx} y1={0} y2={y - 3} />
              <rect x={mx - 2.5} y={BAND + 2} width={5} height={5} />
              <text x={extents[i]![0]} y={y + 8}>
                {m.text}
              </text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
