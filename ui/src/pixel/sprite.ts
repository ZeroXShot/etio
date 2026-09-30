// Pixel art (pure, unit-tested).
//
// A sprite is a grid of palette keys, one character per pixel, "." being
// transparent. Keys map to CSS custom properties (`--px-<key>`, see
// `styles/tokens.css`) so that the same art follows the light and dark
// themes; the key "f" is the current text colour. `Pixel.tsx` draws a
// sprite as SVG rectangles, one per horizontal run of equal pixels.

export interface Sprite {
  readonly w: number;
  readonly h: number;
  /** Row-major palette keys, `null` for transparent pixels. */
  readonly px: readonly (string | null)[];
}

/** Parses rows of palette keys; every row must have the same width. */
export function parse(rows: readonly string[]): Sprite {
  const w = rows[0]?.length ?? 0;
  const px: (string | null)[] = [];
  rows.forEach((row, y) => {
    if (row.length !== w) throw new Error(`sprite row ${y} has ${row.length} pixels, expected ${w}`);
    for (const c of row) px.push(c === "." ? null : c);
  });
  return { w, h: rows.length, px };
}

/** A mutable pixel grid, for art that is easier to draw than to type. */
export class Canvas {
  readonly px: (string | null)[];

  constructor(
    readonly w: number,
    readonly h: number,
  ) {
    this.px = new Array<string | null>(w * h).fill(null);
  }

  /** Sets one pixel; coordinates outside the grid are ignored. */
  set(x: number, y: number, key: string | null): void {
    if (x < 0 || y < 0 || x >= this.w || y >= this.h) return;
    this.px[Math.trunc(y) * this.w + Math.trunc(x)] = key;
  }

  get(x: number, y: number): string | null {
    if (x < 0 || y < 0 || x >= this.w || y >= this.h) return null;
    return this.px[y * this.w + x] ?? null;
  }

  rect(x: number, y: number, w: number, h: number, key: string | null): void {
    for (let j = y; j < y + h; j++) for (let i = x; i < x + w; i++) this.set(i, j, key);
  }

  /** A one-pixel outline. */
  frame(x: number, y: number, w: number, h: number, key: string | null): void {
    for (let i = x; i < x + w; i++) {
      this.set(i, y, key);
      this.set(i, y + h - 1, key);
    }
    for (let j = y; j < y + h; j++) {
      this.set(x, j, key);
      this.set(x + w - 1, j, key);
    }
  }

  /** Bresenham's line between two pixels, both included. */
  line(x0: number, y0: number, x1: number, y1: number, key: string | null): void {
    const dx = Math.abs(x1 - x0);
    const dy = -Math.abs(y1 - y0);
    const sx = x0 < x1 ? 1 : -1;
    const sy = y0 < y1 ? 1 : -1;
    let err = dx + dy;
    for (;;) {
      this.set(x0, y0, key);
      if (x0 === x1 && y0 === y1) return;
      const e2 = 2 * err;
      if (e2 >= dy) {
        err += dy;
        x0 += sx;
      }
      if (e2 <= dx) {
        err += dx;
        y0 += sy;
      }
    }
  }

  /** Copies the opaque pixels of `s` with its top-left corner at (x, y). */
  blit(s: Sprite, x: number, y: number): void {
    for (let j = 0; j < s.h; j++)
      for (let i = 0; i < s.w; i++) {
        const key = s.px[j * s.w + i];
        if (key != null) this.set(x + i, y + j, key);
      }
  }

  sprite(): Sprite {
    return { w: this.w, h: this.h, px: [...this.px] };
  }
}

/** A horizontal run of pixels of one key. */
export interface Run {
  x: number;
  y: number;
  w: number;
  key: string;
}

/** The sprite as maximal horizontal runs, in reading order. */
export function runs(s: Sprite): Run[] {
  const out: Run[] = [];
  for (let y = 0; y < s.h; y++) {
    let x = 0;
    while (x < s.w) {
      const key = s.px[y * s.w + x];
      if (key == null) {
        x++;
        continue;
      }
      let end = x + 1;
      while (end < s.w && s.px[y * s.w + end] === key) end++;
      out.push({ x, y, w: end - x, key });
      x = end;
    }
  }
  return out;
}

/** Places sprites side by side, top-aligned, `gap` pixels apart. */
export function hstack(sprites: readonly Sprite[], gap = 1): Sprite {
  const w = sprites.reduce((a, s) => a + s.w, 0) + gap * Math.max(0, sprites.length - 1);
  const c = new Canvas(w, Math.max(0, ...sprites.map((s) => s.h)));
  let x = 0;
  for (const s of sprites) {
    c.blit(s, x, 0);
    x += s.w + gap;
  }
  return c.sprite();
}
