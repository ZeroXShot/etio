// Server-sent events over `fetch`.
//
// `EventSource` cannot send an `Authorization` header, so a protected API
// would be unreachable with it. This reads the stream with `fetch` instead
// and reconnects with exponential backoff.

export interface SseMessage {
  event: string;
  data: string;
}

/**
 * Incremental parser of the `text/event-stream` format (WHATWG HTML,
 * "Server-sent events"): feed it chunks, get complete messages back.
 */
export class SseParser {
  private buffer = "";
  private event = "";
  private data: string[] = [];

  push(chunk: string): SseMessage[] {
    this.buffer += chunk;
    const out: SseMessage[] = [];
    for (;;) {
      const nl = this.buffer.search(/\r\n|\r|\n/);
      if (nl < 0) break;
      const sep = this.buffer.startsWith("\r\n", nl) ? 2 : 1;
      // A lone trailing "\r" may be the first half of "\r\n": wait for more.
      if (sep === 1 && this.buffer[nl] === "\r" && nl === this.buffer.length - 1) break;
      const line = this.buffer.slice(0, nl);
      this.buffer = this.buffer.slice(nl + sep);
      const msg = this.line(line);
      if (msg) out.push(msg);
    }
    return out;
  }

  private line(line: string): SseMessage | null {
    if (line === "") {
      if (this.data.length === 0) {
        this.event = "";
        return null;
      }
      const msg = { event: this.event || "message", data: this.data.join("\n") };
      this.event = "";
      this.data = [];
      return msg;
    }
    if (line.startsWith(":")) return null; // comment (keep-alive)
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "event") this.event = value;
    else if (field === "data") this.data.push(value);
    return null;
  }
}

export interface Subscription {
  close(): void;
}

/** Subscribes to `url`; `onState` reports whether the stream is connected. */
export function subscribe(
  url: string,
  headers: () => Record<string, string>,
  onMessage: (m: SseMessage) => void,
  onState: (connected: boolean) => void,
): Subscription {
  let closed = false;
  let controller: AbortController | null = null;
  let delay = 1000;

  const run = async () => {
    while (!closed) {
      controller = new AbortController();
      try {
        const res = await fetch(url, {
          headers: { Accept: "text/event-stream", ...headers() },
          signal: controller.signal,
        });
        if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
        onState(true);
        delay = 1000;
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        const parser = new SseParser();
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          for (const m of parser.push(value)) onMessage(m);
        }
      } catch {
        // Network error or abort: fall through to the retry.
      }
      onState(false);
      if (closed) return;
      await new Promise((r) => setTimeout(r, delay * (0.5 + Math.random())));
      delay = Math.min(delay * 2, 30_000);
    }
  };
  void run();
  return {
    close() {
      closed = true;
      controller?.abort();
    },
  };
}
