import { describe, expect, it } from "vitest";
import { LAMP_OFF, lamp, scope, WORDMARK } from "./art";

describe("pixel art", () => {
  it("draws the wordmark with a signal-coloured dot", () => {
    expect([WORDMARK.w, WORDMARK.h]).toEqual([27, 10]);
    expect(WORDMARK.px.filter((k) => k === "s")).toHaveLength(4);
  });

  it("lights lamps in the requested key", () => {
    expect(new Set(lamp("g").px.filter((k) => k !== null))).toEqual(new Set(["g", "q"]));
    expect(LAMP_OFF.px.filter((k) => k !== null).every((k) => k === "l")).toBe(true);
  });

  it("renders the oscilloscope deterministically, with a different screen per mode", () => {
    const quiet = scope("quiet");
    expect(scope("quiet")).toEqual(quiet);
    expect([quiet.w, quiet.h]).toEqual([92, 58]);
    expect(scope("no-signal").px).not.toEqual(quiet.px);
    expect(quiet.px).toContain("w");
  });
});
