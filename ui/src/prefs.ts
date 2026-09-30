// Viewer preferences: colour theme and time zone. They are conveniences of
// one browser, so they live in localStorage, and the UI works the same when
// storage is unavailable.

import { createContext } from "preact";
import { useCallback, useContext, useEffect, useState } from "preact/hooks";

export type Theme = "system" | "paper" | "carbon";
export type Zone = "local" | "utc";

export interface Prefs {
  theme: Theme;
  zone: Zone;
}

const KEY = "etio.prefs";
const DEFAULTS: Prefs = { theme: "system", zone: "local" };

export function loadPrefs(): Prefs {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<Prefs>;
    return {
      theme: raw.theme === "paper" || raw.theme === "carbon" ? raw.theme : DEFAULTS.theme,
      zone: raw.zone === "utc" ? "utc" : DEFAULTS.zone,
    };
  } catch {
    return DEFAULTS;
  }
}

function savePrefs(p: Prefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(p));
  } catch {
    // Storage unavailable: the choice lasts until the page is closed.
  }
}

/** Applies a theme to the document; "system" follows the OS setting. */
export function applyTheme(theme: Theme): void {
  if (theme === "system") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", theme);
}

export function usePrefsState(): [Prefs, (change: Partial<Prefs>) => void] {
  const [prefs, setPrefs] = useState(loadPrefs);
  useEffect(() => applyTheme(prefs.theme), [prefs.theme]);
  const update = useCallback((change: Partial<Prefs>) => {
    setPrefs((p) => {
      const next = { ...p, ...change };
      savePrefs(next);
      return next;
    });
  }, []);
  return [prefs, update];
}

export const PrefsContext = createContext<Prefs>(DEFAULTS);

export function usePrefs(): Prefs {
  return useContext(PrefsContext);
}

/**
 * A counter that changes whenever the rendered colours may have changed
 * (theme choice or OS setting), for canvases that must redraw.
 */
export function usePalette(): number {
  const { theme } = usePrefs();
  const [os, setOs] = useState(0);
  useEffect(() => {
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const on = () => setOs((n) => n + 1);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return os * 4 + ["system", "paper", "carbon"].indexOf(theme);
}
