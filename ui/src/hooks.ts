import type { RefObject } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { ApiError } from "./api";

export interface Loaded<T> {
  data: T | null;
  error: ApiError | Error | null;
  loading: boolean;
}

/**
 * Loads `fetcher()` whenever `deps` change and every `refreshMs` (if set).
 * Keeps the previous data while reloading so views do not flicker.
 */
export function useLoad<T>(fetcher: () => Promise<T>, deps: unknown[], refreshMs?: number): Loaded<T> {
  const [state, setState] = useState<Loaded<T>>({ data: null, error: null, loading: true });
  useEffect(() => {
    let cancelled = false;
    const load = () =>
      fetcher().then(
        (data) => !cancelled && setState({ data, error: null, loading: false }),
        (error: Error) => !cancelled && setState((s) => ({ data: s.data, error, loading: false })),
      );
    void load();
    const timer = refreshMs ? setInterval(load, refreshMs) : undefined;
    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, deps);
  return state;
}

/** The current location hash, e.g. `#/incidents/inc-1`. */
export function useHash(): string {
  const [hash, setHash] = useState(location.hash || "#/");
  useEffect(() => {
    const on = () => setHash(location.hash || "#/");
    addEventListener("hashchange", on);
    return () => removeEventListener("hashchange", on);
  }, []);
  return hash;
}

/**
 * Keyboard shortcuts: `bindings` maps `KeyboardEvent.key` values to actions.
 * Keys typed into form fields and keys with modifiers are left alone.
 */
export function useKeys(bindings: Record<string, (e: KeyboardEvent) => void>, deps: unknown[]): void {
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
      const action = bindings[e.key];
      if (action) {
        e.preventDefault();
        action(e);
      }
    };
    addEventListener("keydown", on);
    return () => removeEventListener("keydown", on);
  }, deps);
}

/** An element's content width, kept current as it resizes. */
export function useWidth<T extends HTMLElement>(fallback = 800): [RefObject<T>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(fallback);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setWidth(Math.max(0, Math.floor(el.clientWidth)));
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width];
}
