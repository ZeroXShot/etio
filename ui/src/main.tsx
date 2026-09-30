import "@fontsource/ibm-plex-sans/latin-400.css";
import "@fontsource/ibm-plex-sans/latin-500.css";
import "@fontsource/ibm-plex-sans/latin-600.css";
import "@fontsource/ibm-plex-mono/latin-400.css";
import "@fontsource/ibm-plex-mono/latin-500.css";
import "@fontsource/ibm-plex-mono/latin-600.css";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/page.css";
import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import { api, authHeaders, type EngineEvent } from "./api";
import { DitherDefs } from "./components/Dither";
import { AppBar, type Section, StatusBar } from "./components/Shell";
import { useHash, useKeys, useLoad } from "./hooks";
import { PrefsContext, usePrefsState } from "./prefs";
import { counters, push, type Sample } from "./rates";
import { subscribe } from "./sse";
import { Engine } from "./views/Engine";
import { IncidentDetail } from "./views/IncidentDetail";
import { Incidents } from "./views/Incidents";
import { ServiceDetail, Services } from "./views/Services";
import { Failure } from "./components/Panel";

/** Status polls kept for rates and sparklines (4.5 minutes at 3 s). */
const HISTORY = 90;

function App() {
  const hash = useHash();
  const [prefs, setPrefs] = usePrefsState();
  const status = useLoad(api.status, [], 3_000);
  const [history, setHistory] = useState<Sample[]>([]);
  const [live, setLive] = useState(false);
  // Bumped on every engine event so that views reload what changed.
  const [version, setVersion] = useState(0);
  const incidents = useLoad(() => api.incidents(), [version], 15_000);

  useEffect(() => {
    const s = status.data;
    if (s) setHistory((h) => push(h, { at: Date.now(), counters: counters(s) }, HISTORY));
  }, [status.data]);

  useEffect(() => {
    const sub = subscribe(
      "/api/v1/events",
      authHeaders,
      (m) => {
        try {
          const e = JSON.parse(m.data) as EngineEvent;
          if (
            e.type === "incident_opened" &&
            typeof Notification !== "undefined" &&
            Notification.permission === "granted"
          ) {
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

  useKeys(
    {
      "1": () => (location.hash = "#/"),
      "2": () => (location.hash = "#/services"),
      "3": () => (location.hash = "#/engine"),
    },
    [],
  );

  const s = status.data;
  const now = s && s.now > 0 ? s.now : Date.now() * 1e6;
  const route = hash.replace(/^#/, "");
  const section: Section = route.startsWith("/services")
    ? "services"
    : route.startsWith("/engine")
      ? "engine"
      : "incidents";
  const list = incidents.data ?? [];
  const open = list.filter((i) => i.status === "open").length;

  let page;
  if (status.error && !s && !route.startsWith("/engine")) {
    page = <Failure error={status.error} />;
  } else if (route.startsWith("/incidents/")) {
    page = <IncidentDetail id={decodeURIComponent(route.slice("/incidents/".length))} now={now} version={version} />;
  } else if (route.startsWith("/services/")) {
    page = (
      <ServiceDetail
        service={decodeURIComponent(route.slice("/services/".length))}
        resolution={s?.resolution_s ?? null}
      />
    );
  } else if (route === "/services") {
    page = <Services />;
  } else if (route === "/engine") {
    page = <Engine status={s ?? null} error={status.error} history={history} />;
  } else if (incidents.error && !incidents.data) {
    page = <Failure error={incidents.error} />;
  } else {
    page = <Incidents incidents={list} now={now} hasTelemetry={(s?.series ?? 0) > 0} />;
  }

  return (
    <PrefsContext.Provider value={prefs}>
      <div class="app">
        <DitherDefs />
        <AppBar section={section} open={open} status={s ?? null} history={history} live={live} />
        {s?.role === "edge" && (
          <p class="edge-banner">
            <span class="label">Edge node</span>
            This node aggregates telemetry and forwards window summaries to the cores, where incidents are detected and
            analysed.
          </p>
        )}
        <main class="page">{page}</main>
        <StatusBar status={s ?? null} prefs={prefs} onPrefs={setPrefs} />
      </div>
    </PrefsContext.Provider>
  );
}

const root = document.getElementById("app");
if (root) render(<App />, root);
