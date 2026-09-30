// Layered layout of the service dependency graph (pure, unit-tested).
//
// Services are placed in columns by their longest distance from an entry
// point (a service nobody calls), so calls flow left to right. Cycles, which
// real call graphs have, are broken by ignoring edges back to a service
// already on the current path.

export interface Placed {
  name: string;
  layer: number;
  row: number;
}

export interface Layout {
  nodes: Map<string, Placed>;
  layers: number;
  rows: number;
}

export function layered(nodes: string[], edges: [string, string, number][]): Layout {
  const out = new Map<string, string[]>();
  const indeg = new Map<string, number>();
  for (const n of nodes) {
    out.set(n, []);
    indeg.set(n, 0);
  }
  for (const [a, b] of edges) {
    if (!out.has(a) || !out.has(b) || a === b) continue;
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

  const byLayer = new Map<number, string[]>();
  for (const [n, l] of layer) {
    if (!byLayer.has(l)) byLayer.set(l, []);
    byLayer.get(l)!.push(n);
  }
  const placed = new Map<string, Placed>();
  let rows = 0;
  for (const [l, names] of byLayer) {
    names.sort();
    names.forEach((name, row) => placed.set(name, { name, layer: l, row }));
    rows = Math.max(rows, names.length);
  }
  return { nodes: placed, layers: byLayer.size ? Math.max(...byLayer.keys()) + 1 : 0, rows };
}
