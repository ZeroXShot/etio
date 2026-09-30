import { describe, expect, it } from "vitest";
import { crossings, layered } from "./layout";

describe("layered", () => {
  it("places services by their longest distance from an entry point", () => {
    const l = layered(
      ["frontend", "cart", "redis", "catalog"],
      [
        ["frontend", "cart", 1],
        ["frontend", "catalog", 1],
        ["cart", "redis", 1],
        ["catalog", "cart", 1],
      ],
    );
    expect(l.nodes.get("frontend")?.layer).toBe(0);
    expect(l.nodes.get("catalog")?.layer).toBe(1);
    expect(l.nodes.get("cart")?.layer).toBe(2);
    expect(l.nodes.get("redis")?.layer).toBe(3);
    expect(l.layers).toBe(4);
  });

  it("routes an edge spanning columns through a slot in each column between", () => {
    const l = layered(
      ["a", "b", "c", "d"],
      [
        ["a", "b", 1],
        ["b", "c", 1],
        ["c", "d", 1],
        ["a", "d", 7],
      ],
    );
    const long = l.routes.find((r) => r.from === "a" && r.to === "d")!;
    expect(long.via.map((p) => p.layer)).toEqual([1, 2]);
    expect(long.weight).toBe(7);
    // The slots are not the services' own.
    for (const p of long.via) {
      for (const n of l.nodes.values()) expect(n.layer === p.layer && n.row === p.row).toBe(false);
    }
  });

  it("orders columns to avoid crossings", () => {
    // Alphabetical order would cross: a→z, b→y.
    const l = layered(
      ["a", "b", "y", "z"],
      [
        ["a", "z", 1],
        ["b", "y", 1],
      ],
    );
    expect(crossings(l)).toBe(0);
    expect(l.nodes.get("z")!.row).toBe(l.nodes.get("a")!.row);
  });

  it("centres short columns", () => {
    const l = layered(
      ["root", "x", "y", "z"],
      [
        ["root", "x", 1],
        ["root", "y", 1],
        ["root", "z", 1],
      ],
    );
    expect(l.rows).toBe(3);
    expect(l.nodes.get("root")!.row).toBe(1);
  });

  it("terminates on cycles, places every node and flags back edges", () => {
    const l = layered(
      ["a", "b", "c"],
      [
        ["a", "b", 1],
        ["b", "c", 1],
        ["c", "b", 1],
      ],
    );
    expect(l.nodes.size).toBe(3);
    expect(l.nodes.get("c")?.layer).toBe(2);
    expect(l.routes.find((r) => r.from === "c")?.back).toBe(true);
  });

  it("handles a pure cycle and ignores unknown edges", () => {
    const l = layered(
      ["x", "y"],
      [
        ["x", "y", 1],
        ["y", "x", 1],
        ["x", "ghost", 1],
      ],
    );
    expect(l.nodes.size).toBe(2);
    expect(l.routes).toHaveLength(2);
  });
});
