import { describe, expect, it } from "vitest";
import { Canvas, hstack, parse, runs } from "./sprite";

describe("sprite", () => {
  it("parses rows of palette keys, dots being transparent", () => {
    const s = parse(["k.s", ".k."]);
    expect([s.w, s.h]).toEqual([3, 2]);
    expect(s.px).toEqual(["k", null, "s", null, "k", null]);
    expect(() => parse(["kk", "k"])).toThrow(/row 1/);
  });

  it("merges equal neighbours into runs", () => {
    expect(runs(parse(["kkk.ss", "..k..."]))).toEqual([
      { x: 0, y: 0, w: 3, key: "k" },
      { x: 4, y: 0, w: 2, key: "s" },
      { x: 2, y: 1, w: 1, key: "k" },
    ]);
  });

  it("draws rectangles, frames and lines, clipped to the grid", () => {
    const c = new Canvas(5, 4);
    c.frame(0, 0, 5, 4, "k");
    c.rect(1, 1, 3, 2, "p");
    c.set(9, 9, "x");
    expect(runs(c.sprite()).map((r) => `${r.x},${r.y},${r.w},${r.key}`)).toEqual([
      "0,0,5,k",
      "0,1,1,k",
      "1,1,3,p",
      "4,1,1,k",
      "0,2,1,k",
      "1,2,3,p",
      "4,2,1,k",
      "0,3,5,k",
    ]);
  });

  it("draws lines that include both ends", () => {
    const c = new Canvas(4, 4);
    c.line(0, 0, 3, 3, "k");
    expect([0, 1, 2, 3].map((i) => c.get(i, i))).toEqual(["k", "k", "k", "k"]);
    c.line(3, 0, 0, 0, "s");
    expect([0, 1, 2, 3].map((i) => c.get(i, 0))).toEqual(["s", "s", "s", "s"]);
  });

  it("copies only opaque pixels", () => {
    const c = new Canvas(3, 1);
    c.rect(0, 0, 3, 1, "k");
    c.blit(parse(["s.s"]), 0, 0);
    expect(c.sprite().px).toEqual(["s", "k", "s"]);
  });

  it("stacks sprites top-aligned with a gap", () => {
    const s = hstack([parse(["k", "k"]), parse(["ss"])], 2);
    expect([s.w, s.h]).toEqual([5, 2]);
    expect(s.px).toEqual(["k", null, null, "s", "s", "k", null, null, null, null]);
  });
});
