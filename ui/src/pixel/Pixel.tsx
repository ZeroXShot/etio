import { useMemo } from "preact/hooks";
import { text } from "./font";
import { runs, type Sprite } from "./sprite";

function fill(key: string): string {
  return key === "f" ? "currentColor" : `var(--px-${key})`;
}

interface Props {
  sprite: Sprite;
  /** Screen pixels per art pixel; an integer keeps edges crisp. */
  scale?: number;
  /** Accessible name; without one the art is decorative. */
  label?: string;
  class?: string;
}

/** Draws a sprite as crisp SVG rectangles, one per run of equal pixels. */
export function Pixel({ sprite, scale = 2, label, class: cls }: Props) {
  const rects = useMemo(() => runs(sprite), [sprite]);
  return (
    <svg
      class={cls ? `pixel ${cls}` : "pixel"}
      width={sprite.w * scale}
      height={sprite.h * scale}
      viewBox={`0 0 ${sprite.w} ${sprite.h}`}
      shape-rendering="crispEdges"
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : "true"}
    >
      {rects.map((r) => (
        <rect key={`${r.x},${r.y}`} x={r.x} y={r.y} width={r.w} height={1} style={{ fill: fill(r.key) }} />
      ))}
    </svg>
  );
}

/** Text set in the bitmap font, e.g. a large numeral. */
export function PixelText({ value, scale = 3, class: cls }: { value: string; scale?: number; class?: string }) {
  const sprite = useMemo(() => text(value), [value]);
  return <Pixel sprite={sprite} scale={scale} label={value} class={cls} />;
}
