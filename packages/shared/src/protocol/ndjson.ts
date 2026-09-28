/**
 * NDJSON framing for the sidecar transport (ADR 0006).
 * Kept transport-agnostic: it only turns bytes into messages and back.
 */

export const NDJSON_DELIMITER = "\n";

/** A frame larger than this is a bug or an attack, not a slow day. */
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;

/**
 * Frame limit for the host <-> sidecar transport.
 *
 * Larger than [`MAX_FRAME_BYTES`] because one sidecar frame may carry every multimodal
 * prompt part (base64 images/PDFs/audio). The worst-case budget is documented in
 * `types/chat.ts` and must stay under this number; the MCP limit is unchanged.
 */
export const MAX_SIDECAR_FRAME_BYTES = 24 * 1024 * 1024;

export class FrameTooLargeError extends Error {
  constructor(byteLength: number, limit: number = MAX_FRAME_BYTES) {
    super(`NDJSON frame of ${byteLength} bytes exceeds the ${limit} byte limit`);
    this.name = "FrameTooLargeError";
  }
}

/**
 * Serialize one frame, refusing to produce a line the reader would drop.
 *
 * The check is on UTF-8 bytes, not JavaScript string length, because the reader's
 * limit is bytes.
 */
export function encodeFrame(payload: unknown, maxFrameBytes: number = MAX_FRAME_BYTES): string {
  const encoded = `${JSON.stringify(payload)}${NDJSON_DELIMITER}`;
  const bytes = Buffer.byteLength(encoded, "utf8");
  if (bytes > maxFrameBytes) throw new FrameTooLargeError(bytes, maxFrameBytes);
  return encoded;
}

/**
 * Incremental decoder: push chunks, get complete messages.
 * `push` never throws for malformed JSON — it reports the bad line and keeps the
 * stream alive, because one bad frame must not kill the agent process.
 */
export class NdjsonDecoder {
  #buffer = "";
  #bytesInBuffer = 0;

  constructor(
    private readonly onMalformed?: (rawLine: string, error: unknown) => void,
    private readonly maxFrameBytes: number = MAX_FRAME_BYTES,
  ) {}

  push(chunk: string): unknown[] {
    const frames: unknown[] = [];
    let start = 0;
    for (;;) {
      const newline = chunk.indexOf(NDJSON_DELIMITER, start);
      if (newline === -1) break;
      this.#append(chunk.slice(start, newline));
      start = newline + 1;
      const line = this.#take();
      if (line.length > 0) {
        const frameBytes = Buffer.byteLength(line, "utf8");
        if (frameBytes > this.maxFrameBytes) {
          this.onMalformed?.(
            `<oversized ${frameBytes}b>`,
            new FrameTooLargeError(frameBytes, this.maxFrameBytes),
          );
        } else {
          this.#decodeInto(line, frames);
        }
      }
    }
    this.#append(chunk.slice(start));
    if (this.#bytesInBuffer > this.maxFrameBytes) {
      const dropped = this.#take();
      this.onMalformed?.(`<oversized ${dropped.length}b>`, new FrameTooLargeError(dropped.length, this.maxFrameBytes));
    }
    return frames;
  }

  /** Flush a trailing line that arrived without its delimiter (process exiting). */
  end(): unknown[] {
    const frames: unknown[] = [];
    const rest = this.#take();
    if (rest.length > 0) {
      const frameBytes = Buffer.byteLength(rest, "utf8");
      if (frameBytes > this.maxFrameBytes) {
        this.onMalformed?.(
          `<oversized ${frameBytes}b>`,
          new FrameTooLargeError(frameBytes, this.maxFrameBytes),
        );
      } else {
        this.#decodeInto(rest, frames);
      }
    }
    return frames;
  }

  get pendingBytes(): number {
    return this.#bytesInBuffer;
  }

  #append(text: string): void {
    this.#buffer += text;
    this.#bytesInBuffer += Buffer.byteLength(text, "utf8");
  }

  #take(): string {
    const line = this.#buffer;
    this.#buffer = "";
    this.#bytesInBuffer = 0;
    return line;
  }

  #decodeInto(line: string, frames: unknown[]): void {
    try {
      frames.push(JSON.parse(line) as unknown);
    } catch (error) {
      this.onMalformed?.(line, error);
    }
  }
}
