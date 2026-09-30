import { describe, expect, it } from "vitest";
import { cells, isSet } from "./dither";

describe("ordered dithering", () => {
  it("sets exactly `level` of the 16 pixels of a tile", () => {
    for (let level = 0; level <= 16; level++) expect(cells(level)).toHaveLength(level);
  });

  it("nests levels, so denser shades contain lighter ones", () => {
    for (let level = 1; level < 16; level++) {
      const lighter = new Set(cells(level).map(([x, y]) => `${x},${y}`));
      const denser = new Set(cells(level + 1).map(([x, y]) => `${x},${y}`));
      for (const c of lighter) expect(denser.has(c)).toBe(true);
    }
  });

  it("repeats every four pixels and gives a checkerboard at half density", () => {
    expect(isSet(8, 1, 2)).toBe(isSet(8, 5, 6));
    for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) expect(isSet(8, x, y)).toBe((x + y) % 2 === 0);
  });
});
