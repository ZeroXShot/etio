import { useMemo } from "preact/hooks";
import type { Graph } from "../api";
import { count } from "../format";
import { layered, type Point } from "../layout";
import { type DitherLevel, ditherFill } from "./Dither";
import "./Topology.css";

/** Surprise (−log10 p) bounds of the dither levels of the heat strip. */
export const SURPRISE_STEPS: [number, DitherLevel][] = [
  [1, 2],
  [3, 4],
  [8, 8],
  [16, 12],
  [Infinity, 16],
];

export function surpriseLevel(s: number): DitherLevel | null {
  if (!(s > 0)) return null;
  for (const [bound, level] of SURPRISE_STEPS) if (s < bound) return level;
  return 16;
}

/** Advance of the 12 px mono face used for node names. */
const CHAR_W = 7.25;
const ARROW = 7;

interface Props {
  graph: Graph;
  /** Services anomalous now (service view). */
  anomalous?: Set<string>;
  /** Series per service (service view). */
  seriesCount?: Map<string, number>;
  /** Peak surprise per service (incident view). */
  surprise?: Record<string, number>;
  /** Rank of each root-cause candidate (incident view). */
  ranks?: Map<string, number>;
  triggers?: Set<string>;
  selected?: string | null;
  onSelect?: (service: string) => void;
  href?: (service: string) => string;
  compact?: boolean;
}

/**
 * The service dependency graph, calls flowing left to right. Line weight
 * grows with the logarithm of the calls observed on an edge; the drawing
 * shrinks to fit narrow containers.
 */
export function Topology({
  graph,
  anomalous,
  seriesCount,
  surprise,
  ranks,
  triggers,
  selected,
  onSelect,
  href,
  compact = false,
}: Props) {
  const W = compact ? 142 : 156;
  const H = compact ? 38 : 42;
  const GX = compact ? 46 : 72;
  const GY = compact ? 12 : 16;
  const layout = useMemo(() => layered(graph.nodes, graph.edges), [graph]);
  const at = (p: Point) => ({ x: p.layer * (W + GX), y: p.row * (H + GY) });
  const width = Math.max(1, layout.layers) * (W + GX) - GX;
  const height = Math.max(1, layout.rows) * (H + GY) - GY;
  const maxCalls = Math.max(1, ...graph.edges.map((e) => e[2]));
  const hot = (n: string) => (surprise ? (surprise[n] ?? 0) > 0 : (anomalous?.has(n) ?? false));
  const BELOW = layout.routes.some((r) => r.back) ? 22 : 0;
  const vw = width + 24;
  const vh = height + BELOW + 34;

  return (
    <div class="topology">
      <svg
        viewBox={`-12 -12 ${vw} ${vh}`}
        width="100%"
        style={{ maxWidth: `${vw}px`, minWidth: `${Math.round(vw * 0.8)}px` }}
        role="img"
        aria-label="Service dependency graph"
      >
        {layout.routes.map((r) => {
          const p = at(layout.nodes.get(r.from)!);
          const q = at(layout.nodes.get(r.to)!);
          const strong = hot(r.from) && hot(r.to);
          const sw = 1 + 1.5 * (Math.log10(1 + r.weight) / Math.log10(1 + maxCalls));
          let d: string;
          let tip: string;
          if (!r.back) {
            // Through the free slots of intermediate columns, then into the callee.
            let x = p.x + W;
            let y = p.y + H / 2;
            d = `M ${x} ${y}`;
            const hop = (x2: number, y2: number) => {
              const mid = (x + x2) / 2;
              d += ` C ${mid} ${y}, ${mid} ${y2}, ${x2} ${y2}`;
              x = x2;
              y = y2;
            };
            for (const v of r.via) {
              const s = at(v);
              hop(s.x, s.y + H / 2);
              d += ` L ${s.x + W} ${s.y + H / 2}`;
              x = s.x + W;
            }
            hop(q.x - ARROW, q.y + H / 2);
            tip = `translate(${q.x} ${q.y + H / 2})`;
          } else {
            // A call back to an earlier column (a cycle): routed below.
            const x1 = p.x + W / 2;
            const y1 = p.y + H;
            const x2 = q.x + W / 2;
            const y2 = q.y + H;
            const low = Math.max(y1, y2) + BELOW;
            d = `M ${x1} ${y1} C ${x1} ${low}, ${x2} ${low}, ${x2} ${y2 + ARROW}`;
            tip = `translate(${x2} ${y2}) rotate(-90)`;
          }
          return (
            <g key={`${r.from}->${r.to}`} class={strong ? "edge strong" : "edge"}>
              <title>{`${r.from} → ${r.to}: ${count(r.weight)} calls observed`}</title>
              <path d={d} style={{ strokeWidth: sw }} />
              <polygon points={`0,0 ${-ARROW},-3.5 ${-ARROW},3.5`} transform={tip} />
            </g>
          );
        })}
        {graph.nodes.map((n) => {
          const place = layout.nodes.get(n);
          if (!place) return null;
          const { x, y } = at(place);
          const s = surprise?.[n] ?? 0;
          const level = surprise ? surpriseLevel(s) : anomalous?.has(n) ? 16 : null;
          const rank = ranks?.get(n);
          const badge = rank !== undefined && rank <= 3;
          const isSel = selected === n;
          const trig = triggers?.has(n);
          const room = Math.floor((W - 18 - (badge ? 30 : 8)) / CHAR_W);
          const name = n.length > room ? `${n.slice(0, room - 1)}…` : n;
          const sub = surprise
            ? s > 0
              ? `surprise ${s.toFixed(1)}`
              : "not anomalous"
            : anomalous?.has(n)
              ? "anomalous now"
              : seriesCount?.has(n)
                ? `${seriesCount.get(n)} series`
                : "";
          const cls = ["node", isSel && "selected", hot(n) && "hot", (onSelect || href) && "interactive"]
            .filter(Boolean)
            .join(" ");
          const body = (
            <g class={cls} transform={`translate(${x} ${y})`}>
              <title>{`${n}${trig ? ", triggered the incident" : ""}${rank ? `, candidate ${rank}` : ""}`}</title>
              <rect class="node-box" width={W} height={H} />
              {level !== null && (
                <rect class="node-heat" x={1} y={1} width={10} height={H - 2} fill={ditherFill("signal", level)} />
              )}
              <text class="node-name" x={18} y={compact ? 16 : 18}>
                {name}
              </text>
              <text class="node-sub" x={18} y={compact ? 30 : 33}>
                {sub}
              </text>
              {badge && (
                <g class="node-rank">
                  <rect x={W - 24} y={0} width={24} height={H} />
                  <text x={W - 12} y={H / 2 + 4}>
                    {rank}
                  </text>
                </g>
              )}
              {trig && (
                <text class="node-trigger" x={0} y={H + 11}>
                  TRIGGER
                </text>
              )}
            </g>
          );
          if (href) {
            return (
              <a key={n} href={href(n)} aria-label={n}>
                {body}
              </a>
            );
          }
          return onSelect ? (
            <g
              key={n}
              role="button"
              tabIndex={0}
              aria-pressed={isSel}
              aria-label={n}
              onClick={() => onSelect(n)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onSelect(n);
                }
              }}
            >
              {body}
            </g>
          ) : (
            <g key={n}>{body}</g>
          );
        })}
      </svg>
    </div>
  );
}

/** The key to the heat strip's dither levels. */
export function SurpriseLegend() {
  const labels = ["< 1", "1–3", "3–8", "8–16", "≥ 16"];
  return (
    <div class="surprise-legend">
      <span class="label">Peak surprise, −log₁₀ p</span>
      {SURPRISE_STEPS.map(([, level], i) => (
        <span key={level} class="surprise-step">
          <svg width="12" height="12" aria-hidden="true">
            <rect x="0.5" y="0.5" width="11" height="11" fill={ditherFill("signal", level)} class="swatch" />
          </svg>
          {labels[i]}
        </span>
      ))}
      <span class="surprise-step">
        <span class="legend-rank">1</span> candidate rank
      </span>
    </div>
  );
}
