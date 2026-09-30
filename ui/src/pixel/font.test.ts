import { describe, expect, it } from "vitest";
import { GLYPH_H, glyph, text } from "./font";

describe("bitmap font", () => {
  it("has glyphs of the font's height and a consistent width per glyph", () => {
    for (const c of "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ%+-/.: ") {
      const g = glyph(c);
      expect(g).toHaveLength(GLYPH_H);
      expect(new Set(g.map((row) => row.length)).size).toBe(1);
    }
  });

  it("maps lowercase to uppercase and unknown characters to a space", () => {
    expect(glyph("a")).toEqual(glyph("A"));
    expect(glyph("é")).toEqual(glyph(" "));
  });

  it("sets text with the requested spacing and key", () => {
    const s = text("82%", "s");
    expect(s.w).toBe(5 * 3 + 2);
    expect(s.h).toBe(GLYPH_H);
    expect(new Set(s.px.filter((k) => k !== null))).toEqual(new Set(["s"]));
    expect(text("1.5", "f", 2).w).toBe(5 + 2 + 2 + 2 + 5);
  });
});
