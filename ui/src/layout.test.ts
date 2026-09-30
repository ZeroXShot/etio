import { describe, expect, it } from "vitest";
import { layered } from "./layout";

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

  it("terminates on cycles and places every node", () => {
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
  });

  it("handles a pure cycle and unknown edges", () => {
    const l = layered(["x", "y"], [["x", "y", 1], ["y", "x", 1], ["x", "ghost", 1]]);
    expect(l.nodes.size).toBe(2);
  });
});
