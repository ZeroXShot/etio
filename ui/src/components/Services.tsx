import { useMemo } from "preact/hooks";
import { api, type Graph, type ServiceView } from "../api";
import { useLoad } from "../hooks";
import { layered } from "../layout";
import { Chart } from "./Chart";
import { Card, Empty, Failure } from "./common";

export function Services() {
  const services = useLoad(api.services, [], 10_000);
  const graph = useLoad(api.graph, [], 30_000);
  if (services.error && !services.data) return <Failure error={services.error} />;
  const list = services.data ?? [];
  const anomalous = new Set(list.filter((s) => s.anomalous.length > 0).map((s) => s.service));
  return (
    <>
      <Card title="Dependency graph">
        {graph.data && graph.data.nodes.length > 0 ? (
          <GraphView graph={graph.data} anomalous={anomalous} />
        ) : (
          <Empty>No calls observed yet: the graph is built from traces.</Empty>
        )}
      </Card>
      <Card title="Services">
        {list.length === 0 ? <Empty>No telemetry received yet.</Empty> : <ServiceTable services={list} />}
      </Card>
    </>
  );
}

function ServiceTable({ services }: { services: ServiceView[] }) {
  return (
    <table class="table">
      <thead>
        <tr>
          <th>Service</th>
          <th class="num">Series</th>
          <th>Anomalous now</th>
        </tr>
      </thead>
      <tbody>
        {services.map((s) => (
          <tr key={s.service} class="link" onClick={() => (location.hash = `#/services/${encodeURIComponent(s.service)}`)}>
            <td>
              <a href={`#/services/${encodeURIComponent(s.service)}`}>{s.service}</a>
            </td>
            <td class="num">{s.series}</td>
            <td>
              {s.anomalous.length === 0 ? (
                <span class="muted">–</span>
              ) : (
                s.anomalous.map((a) => (
                  <span class="chip danger" key={a}>
                    {a}
                  </span>
                ))
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const NODE_W = 140;
const NODE_H = 34;
const GAP_X = 70;
const GAP_Y = 18;

function GraphView({ graph, anomalous }: { graph: Graph; anomalous: Set<string> }) {
  const layout = useMemo(() => layered(graph.nodes, graph.edges), [graph]);
  const pos = (n: string) => {
    const p = layout.nodes.get(n);
    return p ? { x: p.layer * (NODE_W + GAP_X), y: p.row * (NODE_H + GAP_Y) } : { x: 0, y: 0 };
  };
  const width = Math.max(1, layout.layers) * (NODE_W + GAP_X) - GAP_X;
  const height = Math.max(1, layout.rows) * (NODE_H + GAP_Y) - GAP_Y;
  const maxW = Math.max(1e-9, ...graph.edges.map((e) => e[2]));
  return (
    <div class="graph">
      <svg viewBox={`-4 -4 ${width + 8} ${height + 8}`} width={width + 8} height={height + 8} role="img" aria-label="Service dependency graph">
        <defs>
          <marker id="arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="8" markerHeight="8" markerUnits="userSpaceOnUse" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" class="arrowhead" />
          </marker>
        </defs>
        {graph.edges.map(([a, b, w]) => {
          const p = pos(a);
          const q = pos(b);
          const back = q.x <= p.x;
          const x1 = p.x + NODE_W;
          const y1 = p.y + NODE_H / 2;
          const x2 = back ? q.x + NODE_W : q.x;
          const y2 = q.y + NODE_H / 2;
          const mid = back ? Math.max(x1, x2) + 40 : (x1 + x2) / 2;
          return (
            <path
              key={`${a}->${b}`}
              d={`M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`}
              class="edge"
              style={{ strokeWidth: 1 + 2 * (w / maxW) }}
              marker-end="url(#arrow)"
            >
              <title>{`${a} → ${b}`}</title>
            </path>
          );
        })}
        {graph.nodes.map((n) => {
          const p = pos(n);
          return (
            <a key={n} href={`#/services/${encodeURIComponent(n)}`}>
              <rect x={p.x} y={p.y} width={NODE_W} height={NODE_H} rx={6} class={anomalous.has(n) ? "node anomalous" : "node"} />
              <text x={p.x + NODE_W / 2} y={p.y + NODE_H / 2 + 4} text-anchor="middle">
                {n.length > 18 ? `${n.slice(0, 17)}…` : n}
              </text>
            </a>
          );
        })}
      </svg>
    </div>
  );
}

export function ServiceDetail({ service }: { service: string }) {
  const { data, error } = useLoad(() => api.series(service), [service], 30_000);
  if (error && !data) return <Failure error={error} />;
  const series = [...(data ?? [])].sort((a, b) => a.category.localeCompare(b.category) || a.name.localeCompare(b.name));
  return (
    <>
      <p>
        <a href="#/services">← Services</a>
      </p>
      <Card title={service}>
        {series.length === 0 ? (
          <Empty>No series.</Empty>
        ) : (
          <div class="grid">
            {series.map((s) => (
              <SeriesChart key={s.name} service={service} name={s.name} category={s.category} />
            ))}
          </div>
        )}
      </Card>
    </>
  );
}

function SeriesChart({ service, name, category }: { service: string; name: string; category: string }) {
  const { data } = useLoad(() => api.values(service, name), [service, name], 15_000);
  return (
    <div class="series">
      <div class="series-head">
        <strong>{name}</strong> <span class="chip">{category}</span>
      </div>
      {data ? <Chart times={data.times} values={data.values} height={110} label={name} /> : <div class="chart placeholder" />}
    </div>
  );
}
