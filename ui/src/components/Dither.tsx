import { cells } from "../pixel/dither";

/** Colours with dither patterns, as CSS custom properties. */
const COLORS = { signal: "--signal", ink: "--ink", muted: "--ink-3" } as const;
export type DitherColor = keyof typeof COLORS;

/** Densities (out of 16) with a pattern. */
export const LEVELS = [2, 4, 8, 12] as const;
export type DitherLevel = (typeof LEVELS)[number] | 16;

/** Size of one dither pixel, in CSS pixels. */
const CELL = 2;

/**
 * SVG patterns for dithered fills, rendered once per page and referenced
 * with `fill="url(#…)"` (see `ditherFill`) from any inline SVG.
 */
export function DitherDefs() {
  return (
    <svg class="dither-defs" width="0" height="0" aria-hidden="true" focusable="false">
      <defs>
        {Object.entries(COLORS).flatMap(([name, v]) =>
          LEVELS.map((level) => (
            <pattern
              key={`${name}-${level}`}
              id={`dither-${name}-${level}`}
              width={4 * CELL}
              height={4 * CELL}
              patternUnits="userSpaceOnUse"
            >
              {cells(level).map(([x, y]) => (
                <rect
                  key={`${x},${y}`}
                  x={x * CELL}
                  y={y * CELL}
                  width={CELL}
                  height={CELL}
                  style={{ fill: `var(${v})` }}
                />
              ))}
            </pattern>
          )),
        )}
      </defs>
    </svg>
  );
}

/** The fill for a dithered area; level 16 is the solid colour. */
export function ditherFill(color: DitherColor, level: DitherLevel): string {
  return level === 16 ? `var(${COLORS[color]})` : `url(#dither-${color}-${level})`;
}
