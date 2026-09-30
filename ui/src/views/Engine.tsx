import { useMemo } from "preact/hooks";
import type { Status } from "../api";
import { Failure, Loading, Panel, Readout } from "../components/Panel";
import { clock, count, day, formatDuration, rate, zoneLabel } from "../format";
import { Pixel } from "../pixel/Pixel";
import { Canvas } from "../pixel/sprite";
import { usePrefs } from "../prefs";
import { counters, latest, type Sample, series } from "../rates";
import "./Engine.css";

const SPARK_W = 72;
const SPARK_H = 14;

/** A pixel sparkline of a counter's rate over the polls of this page. */
function Spark({ values }: { values: number[] }) {
  const sprite = useMemo(() => {
    const c = new Canvas(SPARK_W, SPARK_H);
    for (let x = 0; x < SPARK_W; x += 2) c.set(x, SPARK_H - 1, "l");
    const v = values.slice(-SPARK_W);
    const max = Math.max(1e-9, ...v);
    let prev: number | null = null;
    v.forEach((y, i) => {
      const x = SPARK_W - v.length + i;
      const py = SPARK_H - 1 - Math.round((y / max) * (SPARK_H - 2));
      if (prev !== null) c.line(x - 1, prev, x, py, "k");
      else c.set(x, py, "k");
      prev = py;
    });
    return c.sprite();
  }, [values]);
  return <Pixel sprite={sprite} scale={2} />;
}

const INGEST: [string, string, string][] = [
  ["spans", "Spans", "Spans ingested over OTLP."],
  ["traces", "Traces", "Traces assembled and analysed once idle for trace_timeout."],
  ["logs", "Log records", "Log records ingested; bodies are clustered into templates."],
  ["metric_points", "Metric points", "Metric data points ingested."],
  ["windows", "Windows closed", "Windows sealed after their end plus lateness, each turned into one value per series."],
  ["analyses", "Analyses", "Root-cause analyses run for open incidents."],
];

const QUALITY: [string, string, string][] = [
  ["late", "Late observations", "Dropped because their window had already closed. Growth means lateness is too short."],
  ["future", "Future observations", "Dropped because their timestamps were too far ahead of the clock."],
  ["skew_adjusted", "Skew-corrected spans", "Shifted into their parent span to correct clock skew between hosts."],
  ["evicted", "Evicted traces", "Released before going idle because the span buffer was full."],
  ["oversized", "Oversized trace chunks", "Released because a single trace exceeded its span budget."],
  [
    "metric_points_unpaired",
    "Unpaired cumulative points",
    "Could not become a rate: first point of a stream, or stream budget reached.",
  ],
  ["analyses_skipped", "Skipped analyses", "Could not run for lack of history."],
];

export function Engine({ status, error, history }: { status: Status | null; error: Error | null; history: Sample[] }) {
  const { zone } = usePrefs();
  if (error && !status) return <Failure error={error} />;
  if (!status) return <Loading />;
  const now = status.now > 0 ? status.now : null;
  const wall = Date.now() * 1e6;
  const lag = now !== null ? (wall - now) / 1e9 : null;
  const values = counters(status);
  const pollSpan = history.length > 1 ? (history[history.length - 1]!.at - history[0]!.at) / 1000 : 0;
  return (
    <>
      <div class="page-head">
        <div class="page-title">
          <h1>Engine</h1>
          <p class="page-sub">
            The streaming engine of this node. Counters are cumulative and survive restarts when snapshots are enabled;
            rates are measured by this page between polls.
          </p>
        </div>
        <div class="page-readouts">
          <Readout label="Version">{status.version}</Readout>
          <Readout label="Role">{status.role}</Readout>
          <Readout label="Event clock" note={now ? `${day(now, zone)} ${zoneLabel(zone, now)}` : undefined}>
            {now ? clock(now, zone) : "–"}
          </Readout>
          <Readout label="Event time lag" note="wall clock minus event clock">
            {lag === null ? "–" : lag >= 0 ? formatDuration(lag) : `${formatDuration(-lag)} ahead`}
          </Readout>
          <Readout label="Window" note={`lateness ${status.lateness_s} s`}>
            {status.resolution_s} s
          </Readout>
          <Readout label="Series">{count(status.series)}</Readout>
        </div>
      </div>
      <Panel
        title="Throughput"
        meta={pollSpan > 0 ? `rates over the last ${formatDuration(pollSpan)} of polls` : "waiting for a second poll"}
        flush
      >
        <table class="table engine-table">
          <thead>
            <tr>
              <th>Counter</th>
              <th class="num">Total</th>
              <th class="num">Rate</th>
              <th>History</th>
              <th>Meaning</th>
            </tr>
          </thead>
          <tbody>
            {INGEST.map(([key, name, help]) => (
              <tr key={key}>
                <td>{name}</td>
                <td class="num mono">{count(values[key] ?? 0)}</td>
                <td class="num mono">{rate(latest(history, key))}</td>
                <td class="spark-cell">
                  <Spark values={series(history, key)} />
                </td>
                <td class="muted">{help}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Panel>
      <Panel title="Data quality" meta="should stay at or near zero" flush>
        <table class="table engine-table">
          <thead>
            <tr>
              <th>Counter</th>
              <th class="num">Total</th>
              <th>Meaning</th>
            </tr>
          </thead>
          <tbody>
            {QUALITY.map(([key, name, help]) => {
              const v = values[key] ?? 0;
              return (
                <tr key={key} class={v > 0 ? "nonzero" : undefined}>
                  <td>{name}</td>
                  <td class="num mono">{count(v)}</td>
                  <td class="muted">{help}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </Panel>
      <Panel title="Endpoints" meta="for probes and scrapers">
        <dl class="endpoints mono">
          <dt>
            <a href="/metrics">/metrics</a>
          </dt>
          <dd>Prometheus metrics of the server, in the OpenMetrics text format.</dd>
          <dt>
            <a href="/healthz">/healthz</a>
          </dt>
          <dd>Liveness: the process answers.</dd>
          <dt>
            <a href="/readyz">/readyz</a>
          </dt>
          <dd>Readiness: the engine answers within two seconds.</dd>
          <dt>
            <a href="/api/v1/status">/api/v1/status</a>
          </dt>
          <dd>The data of this page, as JSON.</dd>
        </dl>
      </Panel>
    </>
  );
}
