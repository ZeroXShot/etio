import { useMemo, useState } from "preact/hooks";
import { api, type ServiceView } from "../api";
import { Chart } from "../components/Chart";
import { Lamp } from "../components/Meter";
import { Empty, Failure, Loading, Panel, Readout } from "../components/Panel";
import { Topology } from "../components/Topology";
import { categoryName, categoryOrder, count, formatDuration, formatValue } from "../format";
import { useKeys, useLoad } from "../hooks";
import "./Services.css";

const href = (s: string) => `#/services/${encodeURIComponent(s)}`;

/** Windows the API returns when no range is given. */
const HISTORY_WINDOWS = 360;

export function Services() {
  const services = useLoad(api.services, [], 10_000);
  const graph = useLoad(api.graph, [], 30_000);
  const list = services.data ?? [];
  const anomalous = useMemo(() => new Set(list.filter((s) => s.anomalous.length > 0).map((s) => s.service)), [list]);
  const seriesCount = useMemo(() => new Map(list.map((s) => [s.service, s.series])), [list]);
  if (services.error && !services.data) return <Failure error={services.error} />;
  if (!services.data) return <Loading />;
  const g = graph.data;
  return (
    <>
      <div class="page-head">
        <div class="page-title">
          <h1>Services</h1>
          <p class="page-sub">
            Every service that sent telemetry, and the calls between them as seen in traces. A service is anomalous
            while one of its series is confirmed anomalous.
          </p>
        </div>
        <div class="page-readouts">
          <Readout label="Services">{list.length}</Readout>
          <Readout label="Anomalous now" tone={anomalous.size > 0 ? "signal" : undefined}>
            {anomalous.size}
          </Readout>
          <Readout label="Series">{count(list.reduce((a, s) => a + s.series, 0))}</Readout>
          <Readout label="Call edges">{g ? g.edges.length : "–"}</Readout>
        </div>
      </div>
      {list.length === 0 ? (
        <Panel title="Services">
          <Empty mode="no-signal" title="No telemetry yet">
            Services appear as soon as the first spans, metrics or logs arrive over OTLP.
          </Empty>
        </Panel>
      ) : (
        <>
          <Panel title="Dependency graph" meta="calls flow left to right · line weight: calls observed" flush>
            {g && g.nodes.length > 0 ? (
              <Topology graph={g} anomalous={anomalous} seriesCount={seriesCount} href={href} />
            ) : (
              <p class="muted graph-empty">No calls observed yet: the graph is built from traces.</p>
            )}
          </Panel>
          <Panel title="All services" meta="sorted by name" flush>
            <ServiceTable services={list} />
          </Panel>
        </>
      )}
    </>
  );
}

function ServiceTable({ services }: { services: ServiceView[] }) {
  const [cursor, setCursor] = useState(-1);
  useKeys(
    {
      j: () => setCursor((c) => Math.min(services.length - 1, c + 1)),
      k: () => setCursor((c) => Math.max(0, c - 1)),
      Enter: () => {
        const s = services[cursor];
        if (s) location.hash = href(s.service);
      },
    },
    [services, cursor],
  );
  return (
    <table class="table service-table">
      <thead>
        <tr>
          <th>Health</th>
          <th>Service</th>
          <th class="num">Series</th>
          <th>Anomalous series now</th>
        </tr>
      </thead>
      <tbody>
        {services.map((s, i) => (
          <tr
            key={s.service}
            class={cursor === i ? "row current" : "row"}
            onClick={() => (location.hash = href(s.service))}
          >
            <td>
              <span class={s.anomalous.length ? "health bad" : "health"}>
                <Lamp tone={s.anomalous.length ? "signal" : "ok"} />
                {s.anomalous.length ? "Anomalous" : "Normal"}
              </span>
            </td>
            <td>
              <a class="mono service-link" href={href(s.service)}>
                {s.service}
              </a>
            </td>
            <td class="num mono">{s.series}</td>
            <td class="mono anomalous-list">
              {s.anomalous.length === 0 ? <span class="muted">–</span> : s.anomalous.join(", ")}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function ServiceDetail({ service, resolution }: { service: string; resolution: number | null }) {
  const series = useLoad(() => api.series(service), [service], 30_000);
  const services = useLoad(api.services, [], 10_000);
  useKeys({ Escape: () => (location.hash = "#/services") }, []);
  const anomalous = useMemo(
    () => new Set(services.data?.find((s) => s.service === service)?.anomalous ?? []),
    [services.data, service],
  );
  const groups = useMemo(() => {
    const by = new Map<string, string[]>();
    for (const s of series.data ?? []) {
      if (!by.has(s.category)) by.set(s.category, []);
      by.get(s.category)!.push(s.name);
    }
    return [...by.entries()]
      .map(([c, names]) => [c, names.sort()] as const)
      .sort((a, b) => categoryOrder(a[0]) - categoryOrder(b[0]));
  }, [series.data]);
  const direction = useMemo(() => new Map((series.data ?? []).map((s) => [s.name, s.direction])), [series.data]);
  if (series.error && !series.data) return <Failure error={series.error} />;
  if (!series.data) return <Loading />;
  return (
    <>
      <nav class="crumbs" aria-label="Breadcrumb">
        <a href="#/services">Services</a>
        <span aria-hidden="true">/</span>
        <span class="mono">{service}</span>
      </nav>
      <div class="page-head">
        <div class="page-title">
          <h1 class="mono">{service}</h1>
          <p class="page-sub">
            Every series of the service over up to the latest {HISTORY_WINDOWS} windows
            {resolution ? ` (${formatDuration(HISTORY_WINDOWS * resolution)})` : ""}, one value per window.
          </p>
        </div>
        <div class="page-readouts">
          <Readout label="Series">{series.data.length}</Readout>
          <Readout label="Anomalous now" tone={anomalous.size > 0 ? "signal" : undefined}>
            {anomalous.size}
          </Readout>
          <Readout label="Categories">{groups.length}</Readout>
        </div>
      </div>
      {groups.length === 0 ? (
        <Panel title="Series">
          <p class="muted">No series.</p>
        </Panel>
      ) : (
        groups.map(([category, names]) => (
          <Panel key={category} title={categoryName(category)} meta={`${names.length} series`}>
            <div class="multiples">
              {names.map((n) => (
                <SeriesCard
                  key={n}
                  service={service}
                  name={n}
                  direction={direction.get(n) ?? "both"}
                  anomalous={anomalous.has(n)}
                />
              ))}
            </div>
          </Panel>
        ))
      )}
    </>
  );
}

const HARMFUL: Record<string, string> = {
  up: "harmful when rising",
  down: "harmful when falling",
  both: "harmful either way",
};

function SeriesCard({
  service,
  name,
  direction,
  anomalous,
}: {
  service: string;
  name: string;
  direction: string;
  anomalous: boolean;
}) {
  const { data } = useLoad(() => api.values(service, name), [service, name], 15_000);
  const last = data ? [...data.values].reverse().find((v) => v !== null) : undefined;
  return (
    <figure class={anomalous ? "multiple anomalous" : "multiple"}>
      <figcaption>
        <span class="multiple-name mono">{name}</span>
        {anomalous && <Lamp tone="signal" label="anomalous now" />}
        <span class="multiple-last mono">{formatValue(last)}</span>
      </figcaption>
      <span class="multiple-dir">{HARMFUL[direction] ?? direction}</span>
      {data ? (
        <Chart times={data.times} values={data.values} height={92} label={name} compact alert={anomalous} />
      ) : (
        <div class="chart-placeholder compact" />
      )}
    </figure>
  );
}
