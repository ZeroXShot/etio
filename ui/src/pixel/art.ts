// The pixel art of the interface: the wordmark, status lamps and the
// oscilloscope shown by empty views. Everything is drawn from code or typed
// row by row, never loaded from image files, so it follows the theme.

import { text } from "./font";
import { Canvas, hstack, parse, type Sprite } from "./sprite";

/** "etio": bitmap letters with the dot of the i in the signal colour. */
export const WORDMARK: Sprite = hstack(
  [
    [".......", ".......", ".......", ".fffff.", "ff...ff", "ff...ff", "fffffff", "ff.....", "ff.....", ".ffffff"],
    [".ff..", ".ff..", ".ff..", "fffff", ".ff..", ".ff..", ".ff..", ".ff..", ".ff..", "..fff"],
    ["ss", "ss", "..", "ff", "ff", "ff", "ff", "ff", "ff", "ff"],
    [".......", ".......", ".......", ".fffff.", "ff...ff", "ff...ff", "ff...ff", "ff...ff", "ff...ff", ".fffff."],
  ].map(parse),
  2,
);

/** A 5×5 indicator lamp lit in palette key `key`, with a glint. */
export function lamp(key: string): Sprite {
  return parse([".xxx.", "xqxxx", "xxxxx", "xxxxx", ".xxx."].map((r) => r.replaceAll("x", key)));
}

/** An unlit lamp: the ring only. */
export const LAMP_OFF: Sprite = parse([".lll.", "l...l", "l...l", "l...l", ".lll."]);

export type ScopeMode = "quiet" | "no-signal";

/** Deterministic noise for the art (a 32-bit LCG), so renders are stable. */
function noise(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

/**
 * A bench oscilloscope. "quiet" traces a flat line (telemetry flows and
 * nothing is wrong); "no-signal" shows static (no telemetry yet).
 */
export function scope(mode: ScopeMode): Sprite {
  const W = 92;
  const H = 58;
  const c = new Canvas(W, H);
  const rnd = noise(mode === "quiet" ? 7 : 11);

  // Cabinet: outline, face, bevel light from the top left.
  c.rect(3, 2, 86, 48, "c");
  c.frame(3, 2, 86, 48, "o");
  for (const [x, y] of [
    [3, 2],
    [88, 2],
    [3, 49],
    [88, 49],
  ] as const)
    c.set(x, y, null);
  c.set(4, 3, "o");
  c.set(87, 3, "o");
  c.set(4, 48, "o");
  c.set(87, 48, "o");
  c.line(5, 3, 86, 3, "h");
  c.line(4, 4, 4, 47, "h");
  c.line(5, 48, 86, 48, "d");
  c.line(87, 4, 87, 47, "d");

  // Screen bezel and tube.
  c.rect(8, 7, 60, 38, "d");
  c.frame(8, 7, 60, 38, "o");
  c.rect(10, 9, 56, 34, "v");
  for (const [x, y] of [
    [10, 9],
    [65, 9],
    [10, 42],
    [65, 42],
  ] as const)
    c.set(x, y, "d");

  // Graticule: a dot every 7 pixels, denser on the centre lines.
  for (let y = 12; y <= 40; y += 7) for (let x = 13; x <= 63; x += 7) c.set(x, y, "n");
  for (let x = 11; x <= 64; x += 2) c.set(x, 26, "n");
  for (let y = 10; y <= 41; y += 2) c.set(38, y, "n");

  if (mode === "quiet") {
    // A flat trace with a little noise, brightest where the beam is.
    let y = 26;
    for (let x = 11; x <= 58; x++) {
      const r = rnd();
      const ny = r < 0.12 ? 25 : r > 0.9 ? 27 : 26;
      c.line(x - 1, y, x, ny, "t");
      y = ny;
    }
    c.set(59, 26, "w");
    c.set(60, 26, "w");
    c.set(59, 25, "t");
    c.set(59, 27, "t");
  } else {
    for (let i = 0; i < 140; i++) c.set(11 + Math.floor(rnd() * 54), 10 + Math.floor(rnd() * 32), "n");
    const label = text("NO SIGNAL", "t");
    c.rect(11, 22, 54, 9, "v");
    c.blit(label, 38 - Math.floor(label.w / 2), 23);
  }

  // Controls: two knobs, a power lamp, a toggle.
  const knob = (cx: number, cy: number, pointer: [number, number]) => {
    const ring = ["..ooo..", ".ohhho.", "ohhhhdo", "ohhhhdo", "ohhhddo", ".odddo.", "..ooo.."];
    c.blit(parse(ring), cx - 3, cy - 3);
    c.line(cx, cy, cx + pointer[0], cy + pointer[1], "o");
  };
  knob(78, 14, [2, -2]);
  knob(78, 28, [-2, -1]);
  c.rect(73, 38, 3, 3, "o");
  c.rect(74, 39, 1, 1, mode === "quiet" ? "g" : "l");
  c.frame(79, 37, 6, 5, "o");
  c.rect(80, 38, 2, 3, "o");
  for (let x = 72; x <= 85; x += 2) c.set(x, 45, "d");

  // Feet.
  c.rect(10, 50, 9, 3, "d");
  c.frame(10, 50, 9, 3, "o");
  c.rect(73, 50, 9, 3, "d");
  c.frame(73, 50, 9, 3, "o");
  return c.sprite();
}
