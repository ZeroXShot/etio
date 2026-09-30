import { useEffect, useState } from "preact/hooks";
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
