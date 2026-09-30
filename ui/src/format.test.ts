import { describe, expect, it } from "vitest";
import { featureName, formatDuration, formatValue, percent } from "./format";

describe("format", () => {
  it("formats durations compactly", () => {
    expect(formatDuration(45)).toBe("45s");
    expect(formatDuration(200)).toBe("3m 20s");
    expect(formatDuration(7500)).toBe("2h 5m");
    expect(formatDuration(3 * 86400 + 4 * 3600)).toBe("3d 4h");
    expect(formatDuration(Number.NaN)).toBe("–");
  });

  it("formats values with SI suffixes", () => {
    expect(formatValue(1234567)).toBe("1.23M");
    expect(formatValue(12345)).toBe("12.3k");
    expect(formatValue(0.1234)).toBe("0.123");
    expect(formatValue(null)).toBe("–");
    expect(formatValue(0)).toBe("0");
  });

  it("names features", () => {
    expect(percent(0.456)).toBe("46%");
    expect(featureName("walk")).toBe("graph random walk");
    expect(featureName("some_new_feature")).toBe("some new feature");
  });
});
