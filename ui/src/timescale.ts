// Time axes for timelines (pure, unit-tested). Times are seconds since the
// epoch here: the precision of the API's nanoseconds is not needed to draw.

/** Candidate tick steps, in seconds. */
const STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400];

/** The smallest step that yields at most `maxTicks` ticks over the range. */
export function tickStep(from: number, to: number, maxTicks: number): number {
  const span = Math.max(1e-9, to - from);
  for (const s of STEPS) if (span / s <= maxTicks) return s;
  return Math.ceil(span / maxTicks / 86400) * 86400;
}

/**
 * Tick times: multiples of the step within [from, to]. Steps are aligned to
 * the epoch, which is aligned to local midnight only for whole-hour
 * offsets; `offsetS` (seconds east of UTC) aligns them to the viewer's zone.
 */
export function ticks(from: number, to: number, maxTicks: number, offsetS = 0): number[] {
  const step = tickStep(from, to, maxTicks);
  const out: number[] = [];
  for (let t = Math.ceil((from + offsetS) / step) * step - offsetS; t <= to; t += step) out.push(t);
  return out;
}

/** A linear map from [from, to] to [0, width]. */
export function scale(from: number, to: number, width: number): (t: number) => number {
  const k = width / Math.max(1e-9, to - from);
  return (t) => (t - from) * k;
}

/**
 * Assigns intervals to lanes so that intervals in one lane do not overlap
 * (greedy by start time, which is optimal for interval graphs). Returns the
 * lane of each interval, in input order.
 */
export function lanes(intervals: readonly (readonly [number, number])[], gap = 0): number[] {
  const order = intervals.map((_, i) => i).sort((a, b) => intervals[a]![0] - intervals[b]![0] || a - b);
  const ends: number[] = [];
  const lane = new Array<number>(intervals.length).fill(0);
  for (const i of order) {
    const [start, end] = intervals[i]!;
    let l = ends.findIndex((e) => e + gap <= start);
    if (l < 0) {
      l = ends.length;
      ends.push(end);
    } else ends[l] = end;
    lane[i] = l;
  }
  return lane;
}

/**
 * Stacks labels anchored at x positions into rows so that no two labels in
 * a row overlap. `widths` are the labels' widths; each label starts at its
 * anchor. Returns the row of each label, in input order.
 */
export function labelRows(xs: readonly number[], widths: readonly number[], gap = 6): number[] {
  return lanes(
    xs.map((x, i) => [x, x + (widths[i] ?? 0)] as const),
    gap,
  );
}
