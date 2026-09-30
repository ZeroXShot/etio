// Layered layout of the service dependency graph (pure, unit-tested).
//
// Services are placed in columns by their longest distance from an entry
// point (a service nobody calls), so calls flow left to right. Cycles, which
// real call graphs have, are broken by ignoring edges back to a service
// already on the current path. An edge that spans several columns passes
// through a virtual node in each column in between, so that it is routed
// through a free slot instead of across a service. Within a column, nodes
// are ordered by the barycentre of their neighbours in the adjacent column
// (Sugiyama's heuristic), sweeping in both directions, which removes most
// crossings.

export interface Placed {
  name: string;
  layer: number;
  /** Vertical slot; columns shorter than the tallest are centred. */
  row: number;
}

/** A slot an edge passes through. */
export interface Point {
  layer: number;
  row: number;
}

export interface Route {
  from: string;
  to: string;
  weight: number;
  /** Slots between the two services, one per intermediate column. */
  via: Point[];
  /** The callee is not to the right of the caller (a cycle). */
  back: boolean;
}

export interface Layout {
  nodes: Map<string, Placed>;
  routes: Route[];
  layers: number;
  rows: number;
}

const SWEEPS = 8;

export function layered(nodes: string[], edges: [string, string, number][]): Layout {
  const out = new Map<string, string[]>();
  const indeg = new Map<string, number>();
  for (const n of nodes) {
    out.set(n, []);
    indeg.set(n, 0);
  }
  const valid = edges.filter(([a, b]) => out.has(a) && out.has(b) && a !== b);
  for (const [a, b] of valid) {
    out.get(a)!.push(b);
    indeg.set(b, (indeg.get(b) ?? 0) + 1);
  }
  for (const list of out.values()) list.sort();

  const layer = new Map<string, number>();
  const onPath = new Set<string>();
  const visit = (n: string, depth: number) => {
    if (onPath.has(n) || (layer.get(n) ?? -1) >= depth) return;
    layer.set(n, depth);
    onPath.add(n);
    for (const m of out.get(n) ?? []) visit(m, depth + 1);
    onPath.delete(n);
  };
  const roots = [...nodes].filter((n) => (indeg.get(n) ?? 0) === 0).sort();
  for (const r of roots) visit(r, 0);
  // Services only reachable through cycles.
  for (const n of [...nodes].sort()) if (!layer.has(n)) visit(n, 0);

  // Split forward edges into one-column segments through virtual nodes.
  const level = new Map(layer);
  const prev = new Map<string, string[]>();
  const next = new Map<string, string[]>();
  const link = (a: string, b: string) => {
    if (!next.has(a)) next.set(a, []);
    if (!prev.has(b)) prev.set(b, []);
    next.get(a)!.push(b);
    prev.get(b)!.push(a);
  };
  const chains: { edge: [string, string, number]; ids: string[]; back: boolean }[] = [];
  for (const e of valid) {
    const [a, b] = e;
    const la = layer.get(a)!;
    const lb = layer.get(b)!;
    if (lb <= la) {
      chains.push({ edge: e, ids: [], back: true });
      continue;
    }
    const ids: string[] = [];
    let from = a;
    for (let l = la + 1; l < lb; l++) {
      const id = `\u0000${a}\u0000${b}\u0000${l}`;
      level.set(id, l);
      ids.push(id);
      link(from, id);
      from = id;
    }
    link(from, b);
    chains.push({ edge: e, ids, back: false });
  }

  const count = level.size ? Math.max(...level.values()) + 1 : 0;
  const columns: string[][] = Array.from({ length: count }, () => []);
  for (const [n, l] of level) columns[l]!.push(n);
  for (const c of columns) c.sort();

  const index = new Map<string, number>();
  const reindex = () => columns.forEach((c) => c.forEach((n, i) => index.set(n, i)));
  reindex();
  const reorder = (col: string[], neighbours: Map<string, string[]>) => {
    const key = new Map<string, number>();
    col.forEach((n, i) => {
      const ns = neighbours.get(n) ?? [];
      key.set(n, ns.length ? ns.reduce((s, m) => s + index.get(m)!, 0) / ns.length : i);
    });
    col.sort((a, b) => key.get(a)! - key.get(b)! || (a < b ? -1 : a > b ? 1 : 0));
  };
  for (let s = 0; s < SWEEPS; s++) {
    if (s % 2 === 0) for (let l = 1; l < count; l++) reorder(columns[l]!, prev);
    else for (let l = count - 2; l >= 0; l--) reorder(columns[l]!, next);
    reindex();
  }

  const rows = Math.max(0, ...columns.map((c) => c.length));
  const slot = new Map<string, Point>();
  columns.forEach((c, l) => {
    const pad = (rows - c.length) / 2;
    c.forEach((id, i) => slot.set(id, { layer: l, row: i + pad }));
  });
  const placed = new Map<string, Placed>();
  for (const n of layer.keys()) placed.set(n, { name: n, ...slot.get(n)! });
  const routes = chains.map(({ edge: [from, to, weight], ids, back }) => ({
    from,
    to,
    weight,
    via: ids.map((id) => slot.get(id)!),
    back,
  }));
  return { nodes: placed, routes, layers: count, rows };
}

/** Number of crossings between route segments spanning adjacent columns. */
export function crossings(layout: Layout): number {
  const segments: [number, number, number][] = [];
  for (const r of layout.routes) {
    if (r.back) continue;
    const pts = [layout.nodes.get(r.from)!, ...r.via, layout.nodes.get(r.to)!];
    for (let i = 1; i < pts.length; i++) segments.push([pts[i - 1]!.layer, pts[i - 1]!.row, pts[i]!.row]);
  }
  let n = 0;
  for (let i = 0; i < segments.length; i++)
    for (let j = i + 1; j < segments.length; j++) {
      const [l1, a1, b1] = segments[i]!;
      const [l2, a2, b2] = segments[j]!;
      if (l1 === l2 && (a1 - a2) * (b1 - b2) < 0) n++;
    }
  return n;
}
