import { describe, expect, it } from "vitest";
import { latest, push, type Sample, series } from "./rates";

const sample = (at: number, spans: number, windows = 1): Sample => ({ at, counters: { spans, windows } });

describe("rates", () => {
  it("keeps a bounded history", () => {
    let h: Sample[] = [];
    for (let i = 0; i < 5; i++) h = push(h, sample(i * 1000, i * 10, i + 1), 3);
    expect(h.map((s) => s.at)).toEqual([2000, 3000, 4000]);
  });

  it("restarts the history when the server's counters reset", () => {
    const h = push([sample(0, 500, 40), sample(1000, 600, 41)], sample(2000, 3, 1), 10);
    expect(h).toHaveLength(1);
  });

  it("computes per-second rates between samples", () => {
    const h = [sample(0, 0), sample(2000, 100), sample(3000, 100), sample(5000, 300)];
    expect(series(h, "spans")).toEqual([50, 0, 100]);
    expect(latest(h, "spans")).toBe(100);
    expect(latest(h.slice(0, 1), "spans")).toBeNull();
  });

  it("reports zero rather than a negative rate", () => {
    expect(series([sample(0, 10), sample(1000, 5)], "spans")).toEqual([0]);
  });
});
