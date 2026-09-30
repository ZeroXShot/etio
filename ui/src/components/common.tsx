import type { ComponentChildren } from "preact";
import { ApiError, token } from "../api";

export function StatusBadge({ status }: { status: "open" | "resolved" }) {
  return <span class={`badge ${status}`}>{status}</span>;
}

export function Card({ title, children, actions }: { title?: string; children: ComponentChildren; actions?: ComponentChildren }) {
  return (
    <section class="card">
      {(title || actions) && (
        <header>
          {title && <h2>{title}</h2>}
          {actions}
        </header>
      )}
      {children}
    </section>
  );
}

export function Bar({ value, max = 1, kind = "accent" }: { value: number; max?: number; kind?: "accent" | "danger" }) {
  const w = max > 0 ? Math.max(0, Math.min(1, value / max)) * 100 : 0;
  return (
    <span class="bar">
      <span class={`fill ${kind}`} style={{ width: `${w}%` }} />
    </span>
  );
}

export function Empty({ children }: { children: ComponentChildren }) {
  return <p class="empty">{children}</p>;
}

/** Error display; a 401 offers to enter the API token. */
export function Failure({ error }: { error: Error }) {
  if (error instanceof ApiError && error.status === 401) {
    const submit = (e: Event) => {
      e.preventDefault();
      const input = (e.currentTarget as HTMLFormElement).elements.namedItem("token") as HTMLInputElement;
      token.set(input.value.trim() || null);
      location.reload();
    };
    return (
      <form class="card token" onSubmit={submit}>
        <h2>API token required</h2>
        <p>This server protects its API. The token is kept for this browser session only.</p>
        <input name="token" type="password" autocomplete="off" placeholder="read token" aria-label="API read token" />
        <button type="submit">Continue</button>
      </form>
    );
  }
  return <p class="error">Error: {error.message}</p>;
}
