import { describe, expect, it } from "vitest";
import { SseParser } from "./sse";

describe("SseParser", () => {
  it("parses named events split across chunks", () => {
    const p = new SseParser();
    expect(p.push("event: incident_opened\nda")).toEqual([]);
    expect(p.push('ta: {"a":1}\n\n')).toEqual([{ event: "incident_opened", data: '{"a":1}' }]);
  });

  it("joins multi-line data, ignores comments and defaults the event name", () => {
    const p = new SseParser();
    expect(p.push(": keep-alive\n\ndata: a\ndata: b\n\n")).toEqual([{ event: "message", data: "a\nb" }]);
  });

  it("accepts CRLF, including a CRLF split between chunks", () => {
    const p = new SseParser();
    expect(p.push("data: x\r")).toEqual([]);
    expect(p.push("\n\r\n")).toEqual([{ event: "message", data: "x" }]);
  });

  it("does not leak the event name into the next message", () => {
    const p = new SseParser();
    const out = p.push("event: a\ndata: 1\n\ndata: 2\n\n");
    expect(out.map((m) => m.event)).toEqual(["a", "message"]);
  });
});
