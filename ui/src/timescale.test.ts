import { describe, expect, it } from "vitest";
import { labelRows, lanes, scale, tickStep, ticks } from "./timescale";

describe("timescale", () => {
  it("picks the smallest round step that fits", () => {
    expect(tickStep(0, 3600, 12)).toBe(300);
    expect(tickStep(0, 3600, 4)).toBe(900);
    expect(tickStep(0, 50, 10)).toBe(5);
    expect(tickStep(0, 10 * 86400, 5)).toBe(2 * 86400);
  });

  it("places ticks on multiples of the step, shifted to the zone", () => {
    expect(ticks(100, 1000, 4)).toEqual([300, 600, 900]);
    // One hour east of UTC: ticks on the local hour.
    expect(ticks(0, 4 * 3600, 4, 3600)).toEqual([0, 3600, 7200, 10800, 14400]);
    expect(ticks(0, 3 * 3600, 3, 1800)).toEqual([1800, 5400, 9000]);
  });

  it("maps times linearly onto a width", () => {
    const x = scale(100, 200, 50);
    expect([x(100), x(150), x(200)]).toEqual([0, 25, 50]);
  });

  it("stacks overlapping intervals into the fewest lanes", () => {
    const l = lanes([
      [0, 10],
      [5, 15],
      [10, 20],
      [12, 14],
    ]);
    expect(l).toEqual([0, 1, 0, 2]);
    expect(
      lanes(
        [
          [0, 10],
          [10.5, 12],
        ],
        1,
      ),
    ).toEqual([0, 1]);
  });

  it("moves overlapping labels to new rows", () => {
    expect(labelRows([0, 30, 200], [50, 50, 50])).toEqual([0, 1, 0]);
  });
});
