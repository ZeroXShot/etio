import { useMemo, useState } from "preact/hooks";
import { api, type Incident, type RankedService, type RcaResult, type Signal } from "../api";
import { durationBetween, featureName, formatTime, formatValue, percent } from "../format";
import { useLoad } from "../hooks";
import { type Band, Chart } from "./Chart";
import { Bar, Card, Empty, Failure, StatusBadge } from "./common";

export function IncidentDetail({ id, now, version }: { id: string; now: number; version: number }) {
  // `version` changes when the event stream reports a new analysis.
  const { data, error } = useLoad(() => api.incident(id), [id, version], 15_000);
  if (error && !data) return <Failure error={error} />;
  if (!data) return <p class="muted">Loading…</p>;
  return <View incident={data} now={now} />;
}

function View({ incident, now }: { incident: Incident; now: number }) {
  const rca = incident.rca;
  const [selected, setSelected] = useState(0);
  const candidate = rca?.ranking[selected] ?? rca?.ranking[0];
  const services = Object.entries(incident.services).sort((a, b) => b[1] - a[1]);
  return (
    <>
      <p>
        <a href="#/">← Incidents</a>
      </p>
      <Card
        title={`Incident ${incident.id}`}
        actions={<StatusBadge status={incident.status} />}
      >
        <dl class="facts">
          <dt>Started</dt>
          <dd>{formatTime(incident.start)}</dd>
          <dt>Duration</dt>
          <dd>{durationBetween(incident.start, incident.resolved_at ?? now)}</dd>
          <dt>Triggered by</dt>
          <dd>
            {incident.trigger.length === 0
              ? "–"
              : incident.trigger.map((s) => (
                  <span class="chip" key={s}>
                    {s}
                    {incident.trigger_signals?.[s] ? ` · ${incident.trigger_signals[s]}` : ""}
                  </span>
                ))}
          </dd>
          <dt>Anomalous services</dt>
          <dd>
            {services.map(([s, surprise]) => (
              <span class="chip" key={s} title={`peak surprise ${surprise.toFixed(1)}`}>
                {s}
              </span>
            ))}
          </dd>
          {rca && (
            <>
              <dt>Analysis</dt>
              <dd>
                {rca.method}
                {rca.model ? ` · model ${rca.model}` : ""} · {incident.analyses} run{incident.analyses === 1 ? "" : "s"}
              </dd>
            </>
          )}
        </dl>
        {rca?.warnings.map((w) => (
          <p class="warning" key={w}>
            {w}
          </p>
        ))}
      </Card>
      {!rca || rca.ranking.length === 0 ? (
        <Card>
          <Empty>The first root-cause analysis runs shortly after the incident opens.</Empty>
        </Card>
      ) : (
        <div class="split">
          <Card title="Root-cause candidates">
            <ol class="ranking">
              {rca.ranking.slice(0, 10).map((r, i) => (
                <li key={r.service} class={i === selected ? "selected" : ""}>
                  <button type="button" onClick={() => setSelected(i)}>
                    <span class="rank">{r.rank}</span>
                    <span class="name">{r.service}</span>
                    <Bar value={r.probability} kind={i === 0 ? "danger" : "accent"} />
                    <span class="num">{percent(r.probability)}</span>
                  </button>
                </li>
              ))}
            </ol>
          </Card>
          {candidate && <Candidate rca={rca} candidate={candidate} />}
        </div>
      )}
      {rca && <FeedbackForm incident={incident} ranking={rca.ranking} />}
    </>
  );
}

function Candidate({ rca, candidate }: { rca: RcaResult; candidate: RankedService }) {
  const contributions = useMemo(
    () =>
      [...candidate.contributions]
        .filter(([, c]) => Math.abs(c) > 1e-3)
        .sort((a, b) => Math.abs(b[1]) - Math.abs(a[1]))
        .slice(0, 8),
    [candidate],
  );
  const maxAbs = Math.max(1e-9, ...contributions.map(([, c]) => Math.abs(c)));
  return (
    <Card title={`Why ${candidate.service}?`}>
      {candidate.reasons.length > 0 && (
        <ul class="reasons">
          {candidate.reasons.map((r) => (
            <li key={r}>{r}</li>
          ))}
        </ul>
      )}
      {contributions.length > 0 && (
        <>
          <h3>Score contributions</h3>
          <table class="contrib">
            <tbody>
              {contributions.map(([f, c]) => (
                <tr key={f}>
                  <td>{featureName(f)}</td>
                  <td class="diverging">
                    <span class="neg">{c < 0 && <span style={{ width: `${(-c / maxAbs) * 100}%` }} />}</span>
                    <span class="pos">{c > 0 && <span style={{ width: `${(c / maxAbs) * 100}%` }} />}</span>
                  </td>
                  <td class="num">{c > 0 ? "+" : ""}{c.toFixed(2)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
      <h3>Evidence</h3>
      {candidate.signals.length === 0 ? (
        <Empty>No anomalous signal of its own: ranked through the dependency graph.</Empty>
      ) : (
        candidate.signals.map((s) => <Evidence key={`${s.service}/${s.name}`} rca={rca} signal={s} />)
      )}
    </Card>
  );
}

function Evidence({ rca, signal }: { rca: RcaResult; signal: Signal }) {
  const [from, to] = [rca.reference[0], rca.abnormal[1]];
  const { data, error } = useLoad(() => api.values(signal.service, signal.name, from, to), [signal.service, signal.name, from, to]);
  const bands: Band[] = useMemo(
    () => [
      { from: rca.reference[0], to: rca.reference[1], kind: "reference" },
      { from: rca.abnormal[0], to: rca.abnormal[1], kind: "abnormal" },
    ],
    [rca],
  );
  const sc = signal.score;
  return (
    <div class="evidence">
      <div class="evidence-head">
        <strong>{signal.name}</strong> <span class="chip">{signal.category}</span>
        <span class="muted">
          {" "}
          peak {formatValue(sc.peak_value)} vs typical {formatValue(sc.reference_median)} · {sc.max.toFixed(1)}σ
          {sc.onset_s !== null ? ` · onset ${sc.onset_s >= 0 ? "+" : ""}${sc.onset_s.toFixed(0)}s` : ""}
        </span>
      </div>
      {error ? (
        <p class="muted">History no longer retained.</p>
      ) : data ? (
        <Chart times={data.times} values={data.values} height={120} bands={bands} marker={rca.anomaly_time} label={signal.name} />
      ) : (
        <div class="chart placeholder" />
      )}
    </div>
  );
}

function FeedbackForm({ incident, ranking }: { incident: Incident; ranking: RankedService[] }) {
  const [state, setState] = useState<"idle" | "sending" | "sent" | string>("idle");
  const submit = async (e: Event) => {
    e.preventDefault();
    const form = e.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    setState("sending");
    try {
      await api.feedback(incident.id, String(data.get("root_cause") ?? ""), String(data.get("comment") ?? ""));
      setState("sent");
    } catch (err) {
      setState((err as Error).message);
    }
  };
  return (
    <Card title="What was the actual cause?">
      {state === "sent" ? (
        <p>Thanks — recorded. Confirmed causes can be exported to evaluate and retrain the ranking model.</p>
      ) : (
        <form class="feedback" onSubmit={submit}>
          <label>
            Root cause
            <input name="root_cause" list="candidates" required maxLength={256} defaultValue={ranking[0]?.service ?? ""} />
            <datalist id="candidates">
              {ranking.map((r) => (
                <option key={r.service} value={r.service} />
              ))}
            </datalist>
          </label>
          <label>
            Comment
            <input name="comment" maxLength={4096} placeholder="optional" />
          </label>
          <button type="submit" disabled={state === "sending"}>
            Record
          </button>
          {state !== "idle" && state !== "sending" && <span class="error">{state}</span>}
        </form>
      )}
    </Card>
  );
}
