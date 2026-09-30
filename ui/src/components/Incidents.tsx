import type { IncidentSummary } from "../api";
import { ago, durationBetween, formatTime, percent } from "../format";
import { Card, Empty, StatusBadge } from "./common";

interface Props {
  incidents: IncidentSummary[];
  now: number;
}

export function Incidents({ incidents, now }: Props) {
  if (incidents.length === 0) {
    return (
      <Card title="Incidents">
        <Empty>
          No incidents. Etio opens one when anomalies persist across services; it needs a few minutes of normal traffic
          to learn baselines first.
        </Empty>
      </Card>
    );
  }
  return (
    <Card title="Incidents">
      <table class="table">
        <thead>
          <tr>
            <th>Status</th>
            <th>Started</th>
            <th>Duration</th>
            <th>Most likely cause</th>
            <th class="num">Services</th>
            <th class="num">Analyses</th>
          </tr>
        </thead>
        <tbody>
          {incidents.map((i) => (
            <tr key={i.id} class="link" onClick={() => (location.hash = `#/incidents/${encodeURIComponent(i.id)}`)}>
              <td>
                <StatusBadge status={i.status} />
              </td>
              <td title={formatTime(i.start)}>{ago(i.start, now)}</td>
              <td>{durationBetween(i.start, i.resolved_at ?? now)}</td>
              <td>
                {i.top ? (
                  <>
                    <a href={`#/incidents/${encodeURIComponent(i.id)}`}>
                      <strong>{i.top[0]}</strong>
                    </a>{" "}
                    <span class="muted">{percent(i.top[1])}</span>
                  </>
                ) : (
                  <span class="muted">analysing…</span>
                )}
              </td>
              <td class="num">{i.services}</td>
              <td class="num">{i.analyses}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}
