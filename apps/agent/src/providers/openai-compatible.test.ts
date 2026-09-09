import assert from "node:assert/strict";
import test from "node:test";

import type { ChatRequest } from "@yukinal/provider-sdk";

import { OpenAiCompatibleProvider } from "./openai-compatible.js";

test("chat SSE flushes a final unterminated data line and closes the stream", async () => {
  const provider = new OpenAiCompatibleProvider({ baseUrl: "http://127.0.0.1:1", model: "test" });
  const request: ChatRequest = {
    model: "test",
    messages: [{ role: "user", content: "hello" }],
  };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('data: {"choices":[{"delta":{"content":"tail"},"finish_reason":null}]}'));
        controller.close();
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );
  try {
    const events = [];
    for await (const event of provider.stream(request)) events.push(event);
    assert.deepEqual(events, [
      { type: "text_delta", text: "tail" },
      { type: "done", finishReason: "stop" },
    ]);
  } finally {
    globalThis.fetch = originalFetch;
  }
});
