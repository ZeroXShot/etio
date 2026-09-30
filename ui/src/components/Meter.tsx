import type { IncidentStatus } from "../api";
import { LAMP_OFF, lamp } from "../pixel/art";
import { Pixel } from "../pixel/Pixel";
import "./Meter.css";

/**
 * A segmented meter, like an LED bar graph: `cells` equal segments, lit in
 * proportion to `value` (0 to 1), rounded to the nearest segment.
 */
export function Meter({
  value,
  cells = 20,
  tone = "ink",
  label,
}: {
  value: number;
  cells?: number;
  tone?: "ink" | "signal";
  label?: string;
}) {
  const v = Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : 0;
  const lit = Math.round(v * cells);
  return (
    <span
      class={`meter ${tone}`}
      role="meter"
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={v}
      aria-label={label}
      style={{ "--cells": cells }}
    >
      {Array.from({ length: cells }, (_, i) => (
        <i key={i} class={i < lit ? "lit" : undefined} />
      ))}
    </span>
  );
}

export type LampTone = "signal" | "ok" | "warn" | "off";

const LAMPS = { signal: lamp("s"), ok: lamp("g"), warn: lamp("a"), off: LAMP_OFF };

/** A pixel indicator lamp. */
export function Lamp({ tone, label }: { tone: LampTone; label?: string }) {
  return <Pixel sprite={LAMPS[tone]} scale={2} label={label} class="lamp" />;
}

/** An incident's status: a lit lamp while open. */
export function StatusMark({ status }: { status: IncidentStatus }) {
  return (
    <span class={`status-mark ${status}`}>
      <Lamp tone={status === "open" ? "signal" : "off"} />
      {status === "open" ? "Open" : "Resolved"}
    </span>
  );
}
