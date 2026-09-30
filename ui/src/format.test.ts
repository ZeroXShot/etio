import { describe, expect, it } from "vitest";
import {
  categoryName,
  categoryOrder,
  clock,
  count,
  day,
  featureName,
  formatDuration,
  formatValue,
  offset,
  percent,
  probability,
  rate,
  signed,
  zoneLabel,
} from "./format";

// 2026-09-30T14:03:12Z in nanoseconds.
const T = Date.UTC(2026, 8, 30, 14, 3, 12) * 1e6;

describe("format", () => {
  it("formats durations compactly", () => {
    expect(formatDuration(45)).toBe("45s");
    expect(formatDuration(200)).toBe("3m 20s");
    expect(formatDuration(7500)).toBe("2h 5m");
    expect(formatDuration(3 * 86400 + 4 * 3600)).toBe("3d 4h");
    expect(formatDuration(Number.NaN)).toBe("–");
  });

  it("formats times in UTC", () => {
    expect(clock(T, "utc")).toBe("14:03:12");
    expect(day(T, "utc")).toBe("2026-09-30");
    expect(zoneLabel("utc", T)).toBe("UTC");
    expect(zoneLabel("local", T)).toMatch(/^UTC[+\u2212]\d\d:\d\d$/);
  });

  it("signs offsets and numbers with a true minus", () => {
    expect(offset(30)).toBe("+30s");
    expect(offset(-5)).toBe("\u22125s");
    expect(offset(560)).toBe("+9m 20s");
    expect(signed(1.449)).toBe("+1.45");
    expect(signed(-2.29)).toBe("\u22122.29");
    expect(signed(-0.001)).toBe("0.00");
  });

  it("formats values with SI suffixes, counts with separators", () => {
    expect(formatValue(1234567)).toBe("1.23M");
    expect(formatValue(12345)).toBe("12.3k");
    expect(formatValue(0.1234)).toBe("0.123");
    expect(formatValue(null)).toBe("–");
    expect(formatValue(0)).toBe("0");
    expect(count(1554186)).toBe("1,554,186");
    expect(count(42)).toBe("42");
    expect(rate(29603)).toBe("29.6k/s");
    expect(rate(null)).toBe("–");
  });

  it("never rounds a small probability to zero", () => {
    expect(percent(0.456)).toBe("46%");
    expect(probability(0.82)).toBe("82%");
    expect(probability(0.004)).toBe("<1%");
    expect(probability(0)).toBe("0%");
  });

  it("names features and categories", () => {
    expect(featureName("walk")).toBe("graph random walk");
    expect(featureName("some_new_feature")).toBe("some new feature");
    expect(categoryName("self_time")).toBe("Own processing time");
    expect(categoryName("new_kind")).toBe("new kind");
    expect(categoryOrder("latency")).toBeLessThan(categoryOrder("logs"));
  });
});
