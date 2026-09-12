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
