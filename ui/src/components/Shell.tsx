import type { ComponentChildren } from "preact";
import { useMemo } from "preact/hooks";
import type { Status } from "../api";
import { clock, count, rate, zoneLabel } from "../format";
import { WORDMARK } from "../pixel/art";
import { Pixel } from "../pixel/Pixel";
import { Canvas } from "../pixel/sprite";
import { type Prefs, type Theme, usePrefs, type Zone } from "../prefs";
import { latest, type Sample, series } from "../rates";
import { Lamp } from "./Meter";
import "./Shell.css";

export type Section = "incidents" | "services" | "engine";

const SCOPE_W = 56;
const SCOPE_H = 13;

/** A pixel bar chart of the span rate over the last polls. */
function IngestScope({ history }: { history: Sample[] }) {
  const sprite = useMemo(() => {
    const c = new Canvas(SCOPE_W, SCOPE_H);
    c.rect(0, 0, SCOPE_W, SCOPE_H, "u");
    for (let x = 1; x < SCOPE_W; x += 2) c.set(x, SCOPE_H - 1, "e");
    const values = series(history, "spans").slice(-(SCOPE_W - 2));
    const max = Math.max(1, ...values);
    values.forEach((v, i) => {
      const h = v > 0 ? Math.max(1, Math.round((v / max) * (SCOPE_H - 2))) : 0;
      const x = SCOPE_W - 1 - values.length + i;
      c.rect(x, SCOPE_H - 1 - h, 1, h, i === values.length - 1 ? "f" : "e");
    });
    return c.sprite();
  }, [history]);
  return <Pixel sprite={sprite} scale={2} class="bar-scope" label="Span rate over the last polls" />;
}

function BarReadout({
  label,
  children,
  secondary = false,
}: {
  label: string;
  children: ComponentChildren;
  /** Hidden first when the bar runs out of room. */
  secondary?: boolean;
}) {
  return (
    <div class={secondary ? "bar-readout secondary" : "bar-readout"}>
      <span>{label}</span>
      <strong>{children}</strong>
    </div>
  );
}

export function AppBar({
  section,
  open,
  status,
  history,
  live,
}: {
  section: Section;
  open: number;
  status: Status | null;
  history: Sample[];
  live: boolean;
}) {
  const { zone } = usePrefs();
  const now = status && status.now > 0 ? status.now : null;
  const link = (s: Section, href: string, name: string, extra?: ComponentChildren) => (
    <a href={href} class={section === s ? "active" : undefined} aria-current={section === s ? "page" : undefined}>
      {name}
      {extra}
    </a>
  );
  return (
    <header class="bar">
      <a class="bar-brand" href="#/" aria-label="Etio, incidents">
        <Pixel sprite={WORDMARK} scale={2} />
      </a>
      <nav class="bar-nav" aria-label="Views">
        {link(
          "incidents",
          "#/",
          "Incidents",
          open > 0 && (
            <span class="bar-count" aria-label={`${open} open`}>
              {open}
            </span>
          ),
        )}
        {link("services", "#/services", "Services")}
        {link("engine", "#/engine", "Engine")}
      </nav>
      <div class="bar-instruments">
        {status && status.role !== "standalone" && <BarReadout label="Role">{status.role}</BarReadout>}
        <BarReadout label="Event clock">{now ? clock(now, zone) : "–"}</BarReadout>
        <BarReadout label="Spans" secondary>
          {rate(latest(history, "spans"))}
        </BarReadout>
        <BarReadout label="Series" secondary>
          {status ? count(status.series) : "–"}
        </BarReadout>
        <IngestScope history={history} />
        <span class="bar-link" title={live ? "Receiving incident events" : "Event stream disconnected; retrying"}>
          <Lamp tone={live ? "ok" : "off"} />
          {live ? "Live" : "Offline"}
        </span>
      </div>
    </header>
  );
}

function Choice<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: [T, string][];
  onChange: (v: T) => void;
}) {
  return (
    <span class="status-choice" role="group" aria-label={label}>
      <span>{label}</span>
      {options.map(([v, name]) => (
        <button key={v} type="button" aria-pressed={v === value} onClick={() => onChange(v)}>
          {name}
        </button>
      ))}
    </span>
  );
}

export function StatusBar({
  status,
  prefs,
  onPrefs,
}: {
  status: Status | null;
  prefs: Prefs;
  onPrefs: (p: Partial<Prefs>) => void;
}) {
  const ref = status && status.now > 0 ? status.now : Date.now() * 1e6;
  return (
    <footer class="statusbar">
      <div class="statusbar-facts">
        {status ? (
          <>
            <span>etio {status.version}</span>
            <span>{status.role}</span>
            <span>window {status.resolution_s} s</span>
            <span>lateness {status.lateness_s} s</span>
            <span>{count(status.stats.windows)} windows</span>
            <span class={status.stats.aggregator.late > 0 ? "warn" : undefined}>
              {count(status.stats.aggregator.late)} late
            </span>
          </>
        ) : (
          <span>not connected</span>
        )}
      </div>
      <div class="statusbar-prefs">
        <Choice<Zone>
          label="Time"
          value={prefs.zone}
          options={[
            ["local", zoneLabel("local", ref)],
            ["utc", "UTC"],
          ]}
          onChange={(zone) => onPrefs({ zone })}
        />
        <Choice<Theme>
          label="Theme"
          value={prefs.theme}
          options={[
            ["system", "System"],
            ["paper", "Paper"],
            ["carbon", "Carbon"],
          ]}
          onChange={(theme) => onPrefs({ theme })}
        />
        <span class="statusbar-keys">
          <kbd>1</kbd>
          <kbd>2</kbd>
          <kbd>3</kbd> views <kbd>j</kbd>
          <kbd>k</kbd> move <kbd>enter</kbd> open <kbd>esc</kbd> back
        </span>
      </div>
    </footer>
  );
}
