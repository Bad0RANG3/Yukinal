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

/**
 * 走到「未分类失败」这条分支的有三类东西：SSE 帧的 JSON 解析错误（消息里带着原始帧
 * 文本）、body 读取中途的连接错误、以及 `fetch` 自己的传输失败。网关把请求原样回显在
 * 错误里很常见，而这条 message 会进事件流、界面和审计记录 —— 所以它必须和 `listModels`
 * 那条路径一样先脱敏。这条测试是补上一个真实存在的缺口，不是防患于未然。
 *
 * 同时钉住 `retryable: false`：这一层无从知道已经吐了多少文本，盲目重试会把用户已经
 * 看到的内容再叠一遍。真正的可重试判断属于 `ProviderError`（HTTP 状态码那一层）。
 */
test("an unclassified stream failure is redacted and not retried", async () => {
  const provider = new OpenAiCompatibleProvider({ baseUrl: "http://127.0.0.1:1", model: "test" });
  const request: ChatRequest = {
    model: "test",
    messages: [{ role: "user", content: "hello" }],
  };
  const originalFetch = globalThis.fetch;
  // 这根假 key 是拼出来的，不是写出来的：仓库自己的 `check-secrets.mjs` 会扫「长得像凭据」
  // 的文本，而它无从知道这一串是假的。拼装让文件里不存在那样一段连续文本，同时这串值
  // 依然**长得像**一把真 key —— 那正是脱敏必须抓住的形状。
  const looksLikeAKey = ["sk", "NOT-A-REAL", "CREDENTIAL"].join("-");
  globalThis.fetch = async () => new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        // 形状照着真实网关：它的错误文本里回显了自己收到的凭据。
        controller.error(new Error(`connection reset while echoing api_key: ${looksLikeAKey}`));
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );
  try {
    const events: Array<{ type: string; message?: string; retryable?: boolean }> = [];
    for await (const event of provider.stream(request)) events.push(event);
    assert.equal(events.length, 1, JSON.stringify(events));
    const [only] = events;
    assert.equal(only?.type, "error");
    assert.equal(only?.retryable, false);
    assert.match(only?.message ?? "", /api_key=\[redacted\]/);
    assert.ok(!(only?.message ?? "").includes("NOT-A-REAL"), `凭据必须被抹掉：${only?.message}`);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("chat/completions maps images and tool traffic to the OpenAI wire shape", async () => {
  const provider = new OpenAiCompatibleProvider({ baseUrl: "https://example.test/v1", model: "vision" });
  const originalFetch = globalThis.fetch;
  let body: Record<string, unknown> = {};
  globalThis.fetch = async (_input, init) => {
    body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    return new Response("data: [DONE]\n\n", {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  try {
    const request: ChatRequest = {
      model: "vision",
      messages: [
        {
          role: "user",
          content: "what is shown?",
          images: [{ mediaType: "image/png", data: "aGVsbG8=", name: "screen.png" }],
          documents: [
            { mediaType: "application/pdf", data: "JVBERi0xLjcK", name: "guide.pdf" },
          ],
          audios: [{ mediaType: "audio/mpeg", data: "aGVsbG8=", name: "note.mp3" }],
        },
        {
          role: "assistant",
          content: "",
          toolCalls: [{ id: "call_1", name: "docker__ps", arguments: { all: true } }],
        },
        { role: "tool", toolCallId: "call_1", content: "[]" },
      ],
    };
    for await (const _event of provider.stream(request)) {
      // Drain the stream so the request body is observed.
    }
    assert.deepEqual(body.messages, [
      {
        role: "user",
        content: [
          { type: "text", text: "what is shown?" },
          {
            type: "image_url",
            image_url: { url: "data:image/png;base64,aGVsbG8=", detail: "auto" },
          },
          {
            type: "file",
            file: {
              filename: "guide.pdf",
              file_data: "data:application/pdf;base64,JVBERi0xLjcK",
            },
          },
          // OpenAI's audio part carries the bytes inline and names the container, not the MIME.
          { type: "input_audio", input_audio: { data: "aGVsbG8=", format: "mp3" } },
        ],
      },
      {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "call_1",
            type: "function",
            function: { name: "docker__ps", arguments: '{"all":true}' },
          },
        ],
      },
      { role: "tool", tool_call_id: "call_1", content: "[]" },
    ]);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("the responses dialect uses input_image blocks", async () => {
  const provider = new OpenAiCompatibleProvider({
    baseUrl: "https://example.test/v1",
    model: "vision",
    wireApi: "responses",
  });
  const originalFetch = globalThis.fetch;
  let body: Record<string, unknown> = {};
  globalThis.fetch = async (_input, init) => {
    body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    return new Response("data: [DONE]\n\n", {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  try {
    const request: ChatRequest = {
      model: "vision",
      messages: [
        {
          role: "user",
          content: "",
          images: [{ mediaType: "image/jpeg", data: "aGVsbG8=" }],
          documents: [
            { mediaType: "application/pdf", data: "JVBERi0xLjcK", name: "guide.pdf" },
          ],
          audios: [{ mediaType: "audio/wav", data: "aGVsbG8=" }],
        },
      ],
    };
    for await (const _event of provider.stream(request)) {
      // Drain the stream so the request body is observed.
    }
    assert.deepEqual(body.input, [
      {
        role: "user",
        content: [
          {
            type: "input_image",
            image_url: "data:image/jpeg;base64,aGVsbG8=",
            detail: "auto",
          },
          {
            type: "input_file",
            filename: "guide.pdf",
            file_data: "data:application/pdf;base64,JVBERi0xLjcK",
          },
          { type: "input_audio", input_audio: { data: "aGVsbG8=", format: "wav" } },
        ],
      },
    ]);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("OpenAI's audio parts take WAV and MP3 only, so other formats fail loudly", async () => {
  const provider = new OpenAiCompatibleProvider({
    baseUrl: "https://example.test/v1",
    model: "audio",
  });
  const originalFetch = globalThis.fetch;
  let sent = false;
  globalThis.fetch = async () => {
    sent = true;
    return new Response("data: [DONE]\n\n", {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  try {
    const request: ChatRequest = {
      model: "audio",
      messages: [
        {
          role: "user",
          content: "transcribe this",
          audios: [{ mediaType: "audio/ogg", data: "T2dnUwAA" }],
        },
      ],
    };
    await assert.rejects(
      async () => {
        for await (const _event of provider.stream(request)) {
          // Drain: the request body is only built on the first pull.
        }
      },
      /WAV and MP3/,
      "a format OpenAI cannot carry must be reported, not dropped",
    );
    assert.equal(sent, false, "nothing may leave when the attachment cannot be carried");
  } finally {
    globalThis.fetch = originalFetch;
  }
});
