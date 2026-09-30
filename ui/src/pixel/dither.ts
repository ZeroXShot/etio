// Ordered dithering (pure, unit-tested).
//
// Shaded areas (analysis windows, anomaly intensity) are drawn as a regular
// pattern of opaque pixels instead of translucent fills or gradients: a
// level of n/16 sets the pixels whose 4×4 Bayer threshold is below n, so
// every level is an even, crisp texture and each level contains the
// previous one.

export const BAYER4: readonly (readonly number[])[] = [
  [0, 8, 2, 10],
  [12, 4, 14, 6],
  [3, 11, 1, 9],
  [15, 7, 13, 5],
];

/** Whether pixel (x, y) is set at density `level`/16. */
export function isSet(level: number, x: number, y: number): boolean {
  return BAYER4[y & 3]![x & 3]! < level;
}

/** The set cells of the 4×4 tile at density `level`/16. */
export function cells(level: number): [number, number][] {
  const out: [number, number][] = [];
  for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) if (isSet(level, x, y)) out.push([x, y]);
  return out;
}

/**
 * A repeating canvas pattern of `color` at density `level`/16, each dither
 * pixel `cell` device pixels wide.
 */
export function canvasPattern(
  ctx: CanvasRenderingContext2D,
  color: string,
  level: number,
  cell: number,
): CanvasPattern | null {
  const tile = document.createElement("canvas");
  tile.width = tile.height = 4 * cell;
  const t = tile.getContext("2d");
  if (!t) return null;
  t.fillStyle = color;
  for (const [x, y] of cells(level)) t.fillRect(x * cell, y * cell, cell, cell);
  return ctx.createPattern(tile, "repeat");
}
