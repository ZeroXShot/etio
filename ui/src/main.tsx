import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import { api, authHeaders, type EngineEvent } from "./api";
import { Failure } from "./components/common";
import { IncidentDetail } from "./components/IncidentDetail";
import { Incidents } from "./components/Incidents";
import { ServiceDetail, Services } from "./components/Services";
import { formatValue } from "./format";
import { useHash, useLoad } from "./hooks";
import { subscribe } from "./sse";
import "./styles.css";

function App() {
  const hash = useHash();
  const status = useLoad(api.status, [], 5_000);
  const [live, setLive] = useState(false);
  // Bumped on every engine event so that views reload what changed.
  const [version, setVersion] = useState(0);
  const incidents = useLoad(() => api.incidents(), [version], 15_000);

  useEffect(() => {
    const sub = subscribe(
      "/api/v1/events",
      authHeaders,
      (m) => {
        try {
          const e = JSON.parse(m.data) as EngineEvent;
          if (e.type === "incident_opened" && Notification?.permission === "granted") {
            new Notification("Etio: new incident", { body: e.incident.trigger.join(", ") });
          }
        } catch {
          // Ignore malformed events.
        }
        setVersion((v) => v + 1);
      },
      setLive,
    );
    return () => sub.close();
  }, []);

  const now = status.data?.now && status.data.now > 0 ? status.data.now : Date.now() * 1e6;
  const route = hash.replace(/^#/, "");
  let page;
  if (status.error && !status.data) {
    page = <Failure error={status.error} />;
  } else if (route.startsWith("/incidents/")) {
    page = <IncidentDetail id={decodeURIComponent(route.slice("/incidents/".length))} now={now} version={version} />;
  } else if (route.startsWith("/services/")) {
    page = <ServiceDetail service={decodeURIComponent(route.slice("/services/".length))} />;
  } else if (route === "/services") {
    page = <Services />;
  } else if (incidents.error && !incidents.data) {
    page = <Failure error={incidents.error} />;
  } else {
    page = <Incidents incidents={incidents.data ?? []} now={now} />;
  }
  const s = status.data;
  const open = (incidents.data ?? []).filter((i) => i.status === "open").length;
  return (
    <>
      <header class="top">
        <a class="brand" href="#/">
          <img src="/favicon.svg" alt="" width="22" height="22" /> Etio
        </a>
        <nav>
          <a href="#/" class={!route.startsWith("/services") ? "active" : ""}>
            Incidents{open > 0 && <span class="count">{open}</span>}
          </a>
          <a href="#/services" class={route.startsWith("/services") ? "active" : ""}>
            Services
          </a>
        </nav>
        <div class="status" title={s ? `Etio ${s.version}` : ""}>
          {s && (
            <>
              {s.role !== "standalone" && <span class="chip">{s.role}</span>}
              <span>{formatValue(s.series)} series</span>
              <span>{formatValue(s.stats.windows)} windows</span>
            </>
          )}
          <span class={live ? "dot live" : "dot"} title={live ? "live updates connected" : "live updates disconnected"} />
        </div>
      </header>
      {s?.role === "edge" && (
        <p class="warning banner">
          This is an edge node: it aggregates telemetry and forwards it to the cores, where incidents are detected and
          analysed.
        </p>
      )}
      <main>{page}</main>
    </>
  );
}

const root = document.getElementById("app");
if (root) render(<App />, root);
