import type { ComponentChildren } from "preact";
import { useMemo } from "preact/hooks";
import { ApiError, token } from "../api";
import { scope, type ScopeMode } from "../pixel/art";
import { Pixel } from "../pixel/Pixel";
import "./Panel.css";

/** A titled section of a view. */
export function Panel({
  title,
  meta,
  children,
  flush = false,
  class: cls,
}: {
  title?: ComponentChildren;
  meta?: ComponentChildren;
  children: ComponentChildren;
  /** No inner padding, for tables and diagrams that reach the edges. */
  flush?: boolean;
  class?: string;
}) {
  return (
    <section class={cls ? `panel ${cls}` : "panel"}>
      {(title || meta) && (
        <header class="panel-head">
          {title && <h2>{title}</h2>}
          {meta && <div class="panel-meta">{meta}</div>}
        </header>
      )}
      <div class={flush ? "panel-body flush" : "panel-body"}>{children}</div>
    </section>
  );
}

/** A labelled value: what it is above, the value, an optional note below. */
export function Readout({
  label,
  children,
  note,
  tone,
}: {
  label: string;
  children: ComponentChildren;
  note?: ComponentChildren;
  tone?: "signal";
}) {
  return (
    <div class={tone ? `readout ${tone}` : "readout"}>
      <span class="label">{label}</span>
      <span class="readout-value">{children}</span>
      {note !== undefined && <span class="readout-note">{note}</span>}
    </div>
  );
}

export function Loading() {
  return (
    <p class="loading label" role="status">
      Loading
      <span class="cursor" aria-hidden="true" />
    </p>
  );
}

/** An empty state, illustrated by the oscilloscope. */
export function Empty({ mode, title, children }: { mode: ScopeMode; title: string; children: ComponentChildren }) {
  const art = useMemo(() => scope(mode), [mode]);
  return (
    <div class="empty">
      <Pixel sprite={art} scale={3} class="empty-art" />
      <div class="empty-text">
        <h3>{title}</h3>
        <p>{children}</p>
      </div>
    </div>
  );
}

/** Error display; a 401 asks for the API token. */
export function Failure({ error }: { error: Error }) {
  if (error instanceof ApiError && error.status === 401) {
    const submit = (e: Event) => {
      e.preventDefault();
      const input = (e.currentTarget as HTMLFormElement).elements.namedItem("token") as HTMLInputElement;
      token.set(input.value.trim() || null);
      location.reload();
    };
    return (
      <form class="panel token-form" onSubmit={submit}>
        <header class="panel-head">
          <h2>API token required</h2>
          <div class="panel-meta">HTTP 401</div>
        </header>
        <div class="panel-body">
          <p>
            This server protects its API with a bearer token. Enter a read token; it is kept for this browser session
            only and sent with every request.
          </p>
          <label class="field">
            <span class="label">Read token</span>
            <input class="input mono" name="token" type="password" autocomplete="off" required />
          </label>
          <button class="button" type="submit">
            Continue
          </button>
        </div>
      </form>
    );
  }
  const status = error instanceof ApiError ? `HTTP ${error.status}` : "network";
  return (
    <div class="failure" role="alert">
      <span class="label">Request failed · {status}</span>
      <p>{error.message || "The server did not answer."}</p>
    </div>
  );
}
