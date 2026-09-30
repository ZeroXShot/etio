import { useMemo, useState } from "preact/hooks";
import { api, type Graph, type Incident, type RankedService, type RcaResult, type Signal } from "../api";
import { type Band, Chart } from "../components/Chart";
import { ditherFill } from "../components/Dither";
import { Meter, StatusMark } from "../components/Meter";
import { Failure, Loading, Panel } from "../components/Panel";
import { IncidentWindows } from "../components/Timeline";
import { SurpriseLegend, Topology } from "../components/Topology";
import {
  categoryName,
  clock,
  day,
  featureName,
  formatDuration,
  formatValue,
  offset,
  probability,
  signed,
  zoneLabel,
} from "../format";
import { useKeys, useLoad } from "../hooks";
import { PixelText } from "../pixel/Pixel";
import { usePrefs } from "../prefs";
import "./IncidentDetail.css";

const S = 1e9;

export function IncidentDetail({ id, now, version }: { id: string; now: number; version: number }) {
  // `version` changes when the event stream reports a new analysis.
  const { data, error } = useLoad(() => api.incident(id), [id, version], 15_000);
  const graph = useLoad(api.graph, [], 60_000);
  useKeys({ Escape: () => (location.hash = "#/") }, []);
  if (error && !data) return <Failure error={error} />;
  if (!data) return <Loading />;
  return <Report incident={data} graph={graph.data} now={now} />;
}

function Report({ incident, graph, now }: { incident: Incident; graph: Graph | null; now: number }) {
  const { zone } = usePrefs();
  const rca = incident.rca;
  const ranking = rca?.ranking ?? [];
  const [selected, setSelected] = useState<string | null>(null);
  const current = ranking.find((r) => r.service === selected) ?? ranking[0];
  const move = (d: number) => {
    if (!current) return;
    const i = ranking.indexOf(current) + d;
    const next = ranking[Math.max(0, Math.min(ranking.length - 1, i))];
    if (next) setSelected(next.service);
  };
  useKeys({ j: () => move(1), k: () => move(-1) }, [ranking, current]);

  const end = incident.resolved_at ?? now;
  const services = Object.entries(incident.services).sort((a, b) => b[1] - a[1]);
  const ranks = useMemo(() => new Map(ranking.map((r) => [r.service, r.rank])), [ranking]);
  const triggers = useMemo(() => new Set(incident.trigger), [incident.trigger]);

  return (
    <>
      <nav class="crumbs" aria-label="Breadcrumb">
        <a href="#/">Incidents</a>
        <span aria-hidden="true">/</span>
        <span class="mono">{incident.id}</span>
      </nav>

      <header class="title-block">
        <div class="tb-cell tb-name">
          <span class="label">Incident</span>
          <h1 class="mono">{incident.id}</h1>
        </div>
        <div class="tb-cell">
          <span class="label">Status</span>
          <StatusMark status={incident.status} />
        </div>
        <div class="tb-cell">
          <span class="label">Onset</span>
          <span class="tb-value">{clock(incident.start, zone)}</span>
          <span class="tb-note">
            {day(incident.start, zone)} {zoneLabel(zone, incident.start)}
          </span>
        </div>
        <div class="tb-cell">
          <span class="label">Detected</span>
          <span class="tb-value">{offset((incident.opened_at - incident.start) / S)}</span>
          <span class="tb-note">at {clock(incident.opened_at, zone)}</span>
        </div>
        <div class="tb-cell">
          <span class="label">Duration</span>
          <span class="tb-value">{formatDuration((end - incident.start) / S)}</span>
          <span class="tb-note">
            {incident.resolved_at !== null ? `resolved ${clock(incident.resolved_at, zone)}` : "ongoing"}
          </span>
        </div>
        <div class="tb-cell">
          <span class="label">Analysis</span>
          <span class="tb-value">{rca ? (rca.model ?? rca.method) : "pending"}</span>
          <span class="tb-note">
            {incident.analyses} run{incident.analyses === 1 ? "" : "s"}
            {rca && rca.skipped_series > 0 ? ` · ${rca.skipped_series} series skipped` : ""}
          </span>
        </div>
        <div class="tb-cell tb-wide">
          <span class="label">Triggered by</span>
          <span class="tb-list">
            {incident.trigger.length === 0
              ? "–"
              : incident.trigger.map((s) => (
                  <span key={s} class="tb-item">
                    <strong>{s}</strong>
                    {incident.trigger_signals?.[s] && <span class="muted"> · {incident.trigger_signals[s]}</span>}
                  </span>
                ))}
          </span>
        </div>
        <div class="tb-cell tb-wide">
          <span class="label">Peak surprise</span>
          <span class="tb-list">
            {services.map(([s, v]) => (
              <span key={s} class="tb-item">
                {s} <span class="muted">{v.toFixed(1)}</span>
              </span>
            ))}
          </span>
        </div>
      </header>

      <Panel
        title="Timeline"
        meta={rca ? "windows compared by the latest analysis" : "the first analysis runs shortly after opening"}
      >
        <IncidentWindows incident={incident} now={now} />
      </Panel>

      {rca?.warnings.map((w) => (
        <p class="notice" key={w}>
          <span class="label">Warning</span> {w}
        </p>
      ))}

      {!rca || ranking.length === 0 || !current ? (
        <Panel title="Root cause">
          <p class="muted">No analysis yet. The first one runs shortly after the incident opens.</p>
        </Panel>
      ) : (
        <div class="report-grid">
          <div class="report-side">
            <Verdict ranking={ranking} />
            <Panel title="Candidates" meta={`${ranking.length} ranked · select to inspect`} flush>
              <Ranking ranking={ranking} current={current} onSelect={setSelected} />
            </Panel>
          </div>
          <div class="report-main">
            {graph && graph.nodes.length > 0 && (
              <Panel title="Propagation" meta="anomalous services on the call graph · calls flow left to right" flush>
                <Topology
                  graph={graph}
                  surprise={incident.services}
                  ranks={ranks}
                  triggers={triggers}
                  selected={current.service}
                  onSelect={setSelected}
                />
                <SurpriseLegend />
              </Panel>
            )}
            <Dossier rca={rca} candidate={current} />
          </div>
        </div>
      )}
      {rca && ranking.length > 0 && <FeedbackForm incident={incident} ranking={ranking} />}
    </>
  );
}

function Verdict({ ranking }: { ranking: RankedService[] }) {
  const [top, next] = ranking;
  if (!top) return null;
  const ratio = next && next.probability > 0 ? top.probability / next.probability : null;
  return (
    <section class="verdict panel">
      <span class="label">Most likely root cause</span>
      <div class="verdict-main">
        <strong class="verdict-service mono">{top.service}</strong>
        <PixelText value={probability(top.probability)} scale={4} class="verdict-p" />
      </div>
      <p class="verdict-note">
        {next ? (
          <>
            {ratio !== null && ratio < 1000 ? `${ratio.toFixed(1)}×` : "far above"} the probability of the next
            candidate, <strong>{next.service}</strong> ({probability(next.probability)}).
          </>
        ) : (
          "The only candidate."
        )}
      </p>
    </section>
  );
}

function Ranking({
  ranking,
  current,
  onSelect,
}: {
  ranking: RankedService[];
  current: RankedService;
  onSelect: (s: string) => void;
}) {
  return (
    <table class="table ranking" aria-label="Root-cause candidates">
      <thead>
        <tr>
          <th class="num">#</th>
          <th>Service</th>
          <th>Probability</th>
          <th class="num">p</th>
          <th class="num">Score</th>
        </tr>
      </thead>
      <tbody>
        {ranking.map((r) => {
          const sel = r === current;
          return (
            <tr
              key={r.service}
              class={sel ? "row selected" : "row"}
              aria-selected={sel}
              tabIndex={0}
              onClick={() => onSelect(r.service)}
              onKeyDown={(e) => e.key === "Enter" && onSelect(r.service)}
            >
              <td class="num mono">{r.rank}</td>
              <td class="mono rank-service">{r.service}</td>
              <td>
                <Meter value={r.probability} cells={16} tone={r.rank === 1 ? "signal" : "ink"} />
              </td>
              <td class="num mono">{probability(r.probability)}</td>
              <td class="num mono">{signed(r.score)}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

const TOP_FEATURES = 8;

function Dossier({ rca, candidate }: { rca: RcaResult; candidate: RankedService }) {
  const [all, setAll] = useState(false);
  const sorted = useMemo(
    () => [...candidate.contributions].sort((a, b) => Math.abs(b[1]) - Math.abs(a[1])),
    [candidate],
  );
  const shown = all ? sorted : sorted.filter(([, c]) => Math.abs(c) >= 1e-3).slice(0, TOP_FEATURES);
  const maxAbs = Math.max(1e-9, ...sorted.map(([, c]) => Math.abs(c)));
  const total = sorted.reduce((s, [, c]) => s + c, 0);
  return (
    <>
      <Panel
        title={
          <>
            Why <span class="mono">{candidate.service}</span>
          </>
        }
        meta={`rank ${candidate.rank} · p ${probability(candidate.probability)} · score ${signed(candidate.score)}`}
      >
        {candidate.reasons.length > 0 && (
          <ol class="findings">
            {candidate.reasons.map((r, i) => (
              <li key={r}>
                <span class="finding-n mono">{String(i + 1).padStart(2, "0")}</span>
                <p>{r}</p>
              </li>
            ))}
          </ol>
        )}
        {sorted.length > 0 && (
          <div class="decomp">
            <div class="subhead">
              <h3>Score decomposition</h3>
              <span class="muted">contribution = weight × standardised feature</span>
            </div>
            <table class="decomp-table">
              <thead>
                <tr>
                  <th>Feature</th>
                  <th class="num">Value</th>
                  <th class="decomp-axis">
                    <span>lowers</span>
                    <span>raises</span>
                  </th>
                  <th class="num">Contribution</th>
                </tr>
              </thead>
              <tbody>
                {shown.map(([f, c]) => (
                  <tr key={f}>
                    <td>{featureName(f)}</td>
                    <td class="num mono muted">{formatValue(candidate.features[f])}</td>
                    <td class="decomp-bar">
                      <Diverging value={c / maxAbs} />
                    </td>
                    <td class="num mono">{signed(c)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div class="decomp-foot">
              <span class="mono">
                Σ {sorted.length} contributions {signed(total)} · offset shared by all candidates{" "}
                {signed(candidate.score - total)} = score {signed(candidate.score)}
              </span>
              {sorted.length > TOP_FEATURES && (
                <button type="button" class="text-button" onClick={() => setAll((a) => !a)}>
                  {all ? `Show the ${TOP_FEATURES} largest` : `Show all ${sorted.length}`}
                </button>
              )}
            </div>
          </div>
        )}
      </Panel>
      <Panel title="Evidence" meta={`${candidate.signals.length} most anomalous series of ${candidate.service}`}>
        {candidate.signals.length === 0 ? (
          <p class="muted">No anomalous series of its own: it ranks through the dependency graph.</p>
        ) : (
          <div class="evidence-list">
            {candidate.signals.map((s) => (
              <Evidence key={`${s.service}/${s.name}`} rca={rca} signal={s} />
            ))}
          </div>
        )}
      </Panel>
    </>
  );
}

/** A horizontal bar from a centre axis, `value` in [-1, 1]. */
function Diverging({ value }: { value: number }) {
  const W = 180;
  const H = 10;
  const half = W / 2;
  const len = Math.round(Math.min(1, Math.abs(value)) * (half - 2));
  return (
    <svg viewBox={`0 0 ${W} ${H}`} width={W} height={H} preserveAspectRatio="none" class="diverging" aria-hidden="true">
      <line
        x1={half + 0.5}
        x2={half + 0.5}
        y1={-2}
        y2={H + 2}
        class="diverging-axis"
        vector-effect="non-scaling-stroke"
      />
      {len > 0 &&
        (value >= 0 ? (
          <rect x={half + 1} y={1} width={len} height={H - 2} class="diverging-pos" />
        ) : (
          <rect x={half - len} y={1} width={len} height={H - 2} fill={ditherFill("ink", 8)} />
        ))}
    </svg>
  );
}

function Evidence({ rca, signal }: { rca: RcaResult; signal: Signal }) {
  const [from, to] = [rca.reference[0], rca.abnormal[1]];
  const { data, error } = useLoad(
    () => api.values(signal.service, signal.name, from, to),
    [signal.service, signal.name, from, to],
  );
  const bands: Band[] = useMemo(
    () => [
      { from: rca.reference[0], to: rca.reference[1], kind: "reference" },
      { from: rca.abnormal[0], to: rca.abnormal[1], kind: "abnormal" },
    ],
    [rca],
  );
  const sc = signal.score;
  const peak = useMemo<[number, number] | undefined>(
    () => (sc.peak_offset_s !== undefined ? [rca.anomaly_time + sc.peak_offset_s, sc.peak_value] : undefined),
    [rca.anomaly_time, sc.peak_offset_s, sc.peak_value],
  );
  const emphasis = useMemo<[number, number]>(() => [rca.abnormal[0], rca.abnormal[1]], [rca]);
  return (
    <article class="evidence">
      <header class="evidence-head">
        <h3 class="mono">{signal.name}</h3>
        <span class="evidence-cat">{categoryName(signal.category)}</span>
        <dl class="evidence-stats">
          <div>
            <dt>peak</dt>
            <dd class="signal">{formatValue(sc.peak_value)}</dd>
          </div>
          <div>
            <dt>typical</dt>
            <dd>{formatValue(sc.reference_median)}</dd>
          </div>
          <div>
            <dt>deviation</dt>
            <dd>{sc.max.toFixed(1)}σ</dd>
          </div>
          <div>
            <dt>sustained</dt>
            <dd>{sc.sustained.toFixed(1)}σ</dd>
          </div>
          <div>
            <dt>onset</dt>
            <dd>{sc.onset_s === null ? "–" : offset(sc.onset_s)}</dd>
          </div>
        </dl>
      </header>
      {error ? (
        <p class="muted evidence-missing">History no longer retained.</p>
      ) : data ? (
        <Chart
          times={data.times}
          values={data.values}
          height={150}
          bands={bands}
          marker={rca.anomaly_time}
          typical={sc.reference_median}
          peak={peak}
          emphasis={emphasis}
          label={signal.name}
        />
      ) : (
        <div class="chart-placeholder" />
      )}
    </article>
  );
}

function FeedbackForm({ incident, ranking }: { incident: Incident; ranking: RankedService[] }) {
  const [state, setState] = useState<"idle" | "sending" | "sent" | { error: string }>("idle");
  const submit = async (e: Event) => {
    e.preventDefault();
    const data = new FormData(e.currentTarget as HTMLFormElement);
    setState("sending");
    try {
      await api.feedback(incident.id, String(data.get("root_cause") ?? ""), String(data.get("comment") ?? ""));
      setState("sent");
    } catch (err) {
      setState({ error: (err as Error).message });
    }
  };
  return (
    <Panel title="Confirm the root cause" meta="stored for evaluation and retraining">
      {state === "sent" ? (
        <p>
          Recorded for <span class="mono">{incident.id}</span> in the <span class="mono">feedback</span> table of the
          server's SQLite store, from which confirmed causes can be exported to evaluate and retrain the ranking model.
        </p>
      ) : (
        <form class="feedback" onSubmit={submit}>
          <label class="field">
            <span class="label">Actual root cause</span>
            <input
              class="input mono"
              name="root_cause"
              list="candidates"
              required
              maxLength={256}
              defaultValue={ranking[0]?.service ?? ""}
            />
            <datalist id="candidates">
              {ranking.map((r) => (
                <option key={r.service} value={r.service} />
              ))}
            </datalist>
          </label>
          <label class="field grow">
            <span class="label">Comment</span>
            <input class="input" name="comment" maxLength={4096} placeholder="What happened, how it was fixed" />
          </label>
          <button class="button" type="submit" disabled={state === "sending"}>
            {state === "sending" ? "Recording" : "Record"}
          </button>
          {typeof state === "object" && (
            <p class="form-error" role="alert">
              {state.error}
            </p>
          )}
        </form>
      )}
    </Panel>
  );
}
