/**
 * Anthropic Messages API adapter tests. Fake `fetch` + constructed SSE bytes only:
 * nothing here touches the network, and nothing here asserts what the real API
 * does — only what this adapter puts on the wire and what it makes of the bytes
 * that come back.
 */

import assert from "node:assert/strict";
import test from "node:test";

import type { ChatRequest, StreamEvent } from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";

import { AnthropicProvider } from "./anthropic.js";

const encoder = new TextEncoder();

type JsonObject = Record<string, unknown>;

interface SeenRequest {
  url: string;
  method: string | undefined;
  headers: Record<string, string>;
  body: Record<string, unknown>;
  signal: AbortSignal | undefined;
}

/** 注入假 fetch：记下适配器发出的请求（URL、方法、头、body、signal），并把脚本化的响应喂回去。 */
function installFetch(handler: (init: RequestInit | undefined) => Response): {
  seen: SeenRequest[];
  restore: () => void;
} {
  const original = globalThis.fetch;
  const seen: SeenRequest[] = [];
  globalThis.fetch = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    seen.push({
      url: typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
      method: init?.method,
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : {},
      signal: init?.signal ?? undefined,
    });
    return handler(init);
  };
  return {
    seen,
    restore: () => {
      globalThis.fetch = original;
    },
  };
}

/** Anthropic 的 SSE 帧带 `event:` 行；适配器按 payload 里的 `type` 分派，带上它正是为了证明这一点。 */
function sseFrames(events: JsonObject[]): string {
  return events.map((event) => `event: ${String(event.type)}\ndata: ${JSON.stringify(event)}\n\n`).join("");
}

function sseResponse(chunks: string[], options: { close?: boolean; signal?: AbortSignal } = {}): Response {
  const { close = true, signal } = options;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      if (close) {
        controller.close();
        return;
      }
      // 真实 fetch 在 signal 中止时会把响应体 error 掉。这里用 signal.reason 复刻 undici 的
      // 行为：一个**永不结束**的流必须能被中止掉，否则测试本身会挂住 —— 而"不许挂住"正是被测的性质。
      signal?.addEventListener(
        "abort",
        () => {
          const reason: unknown = signal.reason;
          controller.error(reason ?? new DOMException("The operation was aborted.", "AbortError"));
        },
        { once: true },
      );
    },
  });
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), { status, headers: { "content-type": "application/json" } });
}

function provider(overrides: Partial<ConstructorParameters<typeof AnthropicProvider>[0]> = {}): AnthropicProvider {
  return new AnthropicProvider({
    baseUrl: "https://api.anthropic.com",
    model: "claude-sonnet-4-5",
    apiKey: "sk-ant-test",
    ...overrides,
  });
}

function chatRequest(overrides: Partial<ChatRequest> = {}): ChatRequest {
  return { model: "claude-sonnet-4-5", messages: [{ role: "user", content: "看看 docker" }], ...overrides };
}

/** 跑完一次流：注入 fetch、收集全部事件、恢复全局 fetch。 */
async function collectStream(
  subject: AnthropicProvider,
  request: ChatRequest,
  handler: (init: RequestInit | undefined) => Response,
): Promise<{ events: StreamEvent[]; seen: SeenRequest[] }> {
  const { seen, restore } = installFetch(handler);
  try {
    const events: StreamEvent[] = [];
    for await (const event of subject.stream(request)) events.push(event);
    return { events, seen };
  } finally {
    restore();
  }
}

const DOCKER_PS_TOOL = {
  type: "function" as const,
  function: {
    name: "docker__ps",
    description: "列出容器",
    parameters: { type: "object", properties: { all: { type: "boolean" } } },
  },
};

test("stream() sends the Messages API shape: top-level system, content blocks, input_schema tools", async () => {
  const request = chatRequest({
    messages: [
      { role: "system", content: "你是 Yukinal。" },
      { role: "user", content: "看看 docker" },
      { role: "assistant", content: "", toolCalls: [{ id: "toolu_1", name: "docker__ps", arguments: { all: true } }] },
      { role: "tool", toolCallId: "toolu_1", content: "CONTAINER ID   IMAGE\nabc123" },
      { role: "assistant", content: "有三个容器" },
    ],
    tools: [DOCKER_PS_TOOL],
  });

  const { seen } = await collectStream(provider(), request, () => sseResponse([sseFrames([{ type: "message_stop" }])]));
  const sent = seen[0];
  assert(sent, "the adapter must send exactly one request");

  assert.equal(sent.url, "https://api.anthropic.com/v1/messages");
  assert.equal(sent.method, "POST");
  assert.equal(sent.headers["content-type"], "application/json");
  assert.equal(sent.headers["x-api-key"], "sk-ant-test");
  assert.equal(sent.headers["anthropic-version"], "2023-06-01");

  // system 是顶层字段，不是 messages 数组里的一个 role —— 这是这个适配器存在的理由。
  assert.equal(sent.body.system, "你是 Yukinal。");
  assert.equal(sent.body.stream, true);
  assert.equal(sent.body.model, "claude-sonnet-4-5");
  // Messages API 的 max_tokens 是必填的，缺省值必须在 body 里出现。
  assert.equal(sent.body.max_tokens, 4096);
  assert.equal(sent.body.temperature, 0);

  // 工具：OpenAI 的 `parameters` 变成 Anthropic 的 `input_schema`，名字原样透传。
  assert.deepEqual(sent.body.tools, [
    { name: "docker__ps", description: "列出容器", input_schema: DOCKER_PS_TOOL.function.parameters },
  ]);

  assert.deepEqual(sent.body.messages, [
    { role: "user", content: [{ type: "text", text: "看看 docker" }] },
    { role: "assistant", content: [{ type: "tool_use", id: "toolu_1", name: "docker__ps", input: { all: true } }] },
    { role: "user", content: [{ type: "tool_result", tool_use_id: "toolu_1", content: "CONTAINER ID   IMAGE\nabc123" }] },
    { role: "assistant", content: [{ type: "text", text: "有三个容器" }] },
  ]);

  const translated = sent.body.messages as Array<{ role: string }>;
  assert.equal(translated.some((message) => message.role === "system"), false, "no system role may leak into messages");
});

test("request-level model, max_tokens, temperature and apiVersion override the defaults", async () => {
  const subject = provider({ model: "claude-sonnet-4-5", apiVersion: "2026-01-01" });
  const request = chatRequest({ model: "claude-haiku-4-5", maxOutputTokens: 8192, temperature: 0.7 });

  const { seen } = await collectStream(subject, request, () => sseResponse([sseFrames([{ type: "message_stop" }])]));
  const sent = seen[0];
  assert(sent);

  assert.equal(sent.body.model, "claude-haiku-4-5", "the request's model wins over the configured one");
  assert.equal(sent.body.max_tokens, 8192);
  assert.equal(sent.body.temperature, 0.7);
  assert.equal(sent.headers["anthropic-version"], "2026-01-01");
});

test("a custom credential header cannot replace the configured key, and the version header stays ours", async () => {
  const subject = provider({
    customHeaders: { "X-Api-Key": "attacker", Authorization: "Bearer attacker", "anthropic-version": "1999-01-01", "X-Trace": "1" },
  });

  const { seen } = await collectStream(subject, chatRequest(), () => sseResponse([sseFrames([{ type: "message_stop" }])]));
  const sent = seen[0];
  assert(sent);

  assert.equal(sent.headers["x-api-key"], "sk-ant-test");
  assert.equal(sent.headers["X-Api-Key"], undefined);
  assert.equal(sent.headers.Authorization, undefined);
  assert.equal(sent.headers["X-Trace"], "1", "non-credential custom headers survive");
  assert.equal(sent.headers["anthropic-version"], "2023-06-01");
});

test("a text stream emits text deltas, one combined usage event, then done", async () => {
  const chunks = [
    sseFrames([
      { type: "message_start", message: { usage: { input_tokens: 12, output_tokens: 1 } } },
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
      { type: "ping" },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "你好" } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "，世界" } },
      { type: "content_block_stop", index: 0 },
      { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 7 } },
      { type: "message_stop" },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    { type: "text_delta", text: "你好" },
    { type: "text_delta", text: "，世界" },
    // 输入 token 只在 message_start 里、输出 token 只在 message_delta 里，所以这一条只能等到流末。
    { type: "usage", inputTokens: 12, outputTokens: 7 },
    { type: "done", finishReason: "stop" },
  ]);
});

test("extended-thinking deltas surface as reasoning_delta, and signatures are dropped", async () => {
  const chunks = [
    sseFrames([
      { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "先看看有没有容器。" } },
      { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: "ErUBCkYIBRgCIkA" } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "有 3 个容器" } },
      { type: "message_stop" },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    // 思考增量不是回答：它走 reasoning_delta（非文本路径），而签名没有可显示文本。
    { type: "reasoning_delta", text: "先看看有没有容器。" },
    { type: "text_delta", text: "有 3 个容器" },
    { type: "done", finishReason: "stop" },
  ]);
});

test("fragmented input_json_delta reassembles into one tool_call with a verbatim tool name", async () => {
  const chunks = [
    sseFrames([
      { type: "message_start", message: { usage: { input_tokens: 8, output_tokens: 1 } } },
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "我来看看。" } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "tool_use", id: "toolu_01", name: "docker__ps", input: {} } },
      { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: '{"al' } },
      { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: 'l":tr' } },
      { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: "ue}" } },
      { type: "content_block_stop", index: 1 },
      { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 21 } },
      { type: "message_stop" },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    { type: "text_delta", text: "我来看看。" },
    // 分片按块下标累加后解析；名字 `docker__ps` 原样通过（适配器里没有任何点名转换）。
    { type: "tool_call", call: { id: "toolu_01", name: "docker__ps", arguments: { all: true } } },
    { type: "usage", inputTokens: 8, outputTokens: 21 },
    { type: "done", finishReason: "tool_calls" },
  ]);
});

test("stop_reason maps onto FinishReason", async () => {
  const cases: Array<[string | null, string]> = [
    ["end_turn", "stop"],
    ["stop_sequence", "stop"],
    ["max_tokens", "length"],
    ["tool_use", "tool_calls"],
    ["refusal", "stop"],
    ["something_the_api_added_later", "stop"],
    [null, "stop"],
  ];

  for (const [reason, expected] of cases) {
    const { events } = await collectStream(provider(), chatRequest(), () =>
      sseResponse([
        sseFrames([
          { type: "message_delta", delta: { stop_reason: reason }, usage: { output_tokens: 2 } },
          { type: "message_stop" },
        ]),
      ]),
    );
    assert.deepEqual(events.at(-1), { type: "done", finishReason: expected }, `stop_reason=${String(reason)}`);
  }
});

test("EOF without message_stop still releases the assembled tool call", async () => {
  // 连接在最后一个事件之后被掐断：没有 message_stop，最后一个 data 行也没有结尾换行。
  const body = sseFrames([
    { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "toolu_9", name: "docker__ps", input: {} } },
    { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: '{"all":true}' } },
  ]).trimEnd();

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse([body]));

  assert.deepEqual(events, [
    { type: "tool_call", call: { id: "toolu_9", name: "docker__ps", arguments: { all: true } } },
    { type: "done", finishReason: "stop" },
  ]);
});

test("a nameless tool_use is dropped and unparseable arguments degrade to { raw }", async () => {
  const chunks = [
    sseFrames([
      { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "toolu_x", name: "", input: {} } },
      { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: '{"a":' } },
      { type: "content_block_stop", index: 0 },
      { type: "content_block_start", index: 1, content_block: { type: "tool_use", id: "toolu_y", name: "shell__run", input: {} } },
      { type: "content_block_delta", index: 1, delta: { type: "input_json_delta", partial_json: "not json" } },
      { type: "content_block_stop", index: 1 },
      { type: "message_stop" },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    // 没有名字的调用不发（模型会看见一个无法调用的工具）；坏参数保留原文而不是丢掉整次调用。
    { type: "tool_call", call: { id: "toolu_y", name: "shell__run", arguments: { raw: "not json" } } },
    { type: "done", finishReason: "stop" },
  ]);
});

test("a mid-stream protocol error surfaces sanitized and non-retryable, with no done after it", async () => {
  const chunks = [
    sseFrames([
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "部分回答" } },
      { type: "error", error: { type: "overloaded_error", message: "Overloaded (api key: ****g2z5)" } },
      { type: "message_stop" },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events[0], { type: "text_delta", text: "部分回答" });
  const error = events.at(-1);
  assert(error && error.type === "error", "the stream must end in an error event");
  assert.equal(error.retryable, false);
  assert.match(error.message, /Overloaded/);
  assert.doesNotMatch(error.message, /g2z5/);
  assert.match(error.message, /\[redacted\]/);
  assert.equal(events.some((event) => event.type === "done"), false);
});

test("cancellation aborts the in-flight request and ends as done(cancelled)", { timeout: 5_000 }, async () => {
  const controller = new AbortController();
  const { seen, restore } = installFetch((init) =>
    sseResponse(
      [
        sseFrames([
          { type: "message_start", message: { usage: { input_tokens: 3, output_tokens: 0 } } },
          { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "半句话" } },
        ]),
      ],
      { close: false, signal: init?.signal ?? undefined },
    ),
  );

  try {
    const iterator = provider().stream(chatRequest({ signal: controller.signal }))[Symbol.asyncIterator]();
    const first = await iterator.next();
    assert.deepEqual(first.value, { type: "text_delta", text: "半句话" });

    controller.abort();

    const rest: StreamEvent[] = [];
    for (;;) {
      const step = await iterator.next();
      if (step.done) break;
      rest.push(step.value);
    }

    assert.deepEqual(rest, [{ type: "done", finishReason: "cancelled" }]);
    assert.equal(seen[0]?.signal?.aborted, true, "the abort must reach the in-flight request");
  } finally {
    restore();
  }
});

test("a stalled stream times out as an error instead of hanging", { timeout: 5_000 }, async () => {
  const { events } = await collectStream(
    provider(),
    chatRequest({ timeoutMs: 30 }),
    (init) =>
      sseResponse([sseFrames([{ type: "message_start", message: { usage: { input_tokens: 3, output_tokens: 0 } } }])], {
        close: false,
        signal: init?.signal ?? undefined,
      }),
  );

  const error = events.at(-1);
  assert(error && error.type === "error", "a timeout is an error event, never a silent hang");
  assert.equal(error.retryable, false);
  assert.match(error.message, /timed out/);
});

test("an HTTP failure is a retryable ProviderError that never echoes the response body", async () => {
  const { restore } = installFetch(
    () => new Response('{"error":{"message":"Your api key: ****g2z5 is invalid"}}', { status: 500 }),
  );

  try {
    await assert.rejects(
      async () => {
        for await (const event of provider().stream(chatRequest())) assert.fail(`unexpected event ${event.type}`);
      },
      (error: unknown) => {
        assert(error instanceof ProviderError);
        assert.equal(error.retryable, true, "5xx is retryable");
        assert.equal(error.status, 500);
        assert.match(error.message, /failed \(500\)/);
        assert.doesNotMatch(error.message, /g2z5/);
        return true;
      },
    );
  } finally {
    restore();
  }
});

test("listModels() authenticates the catalog request and maps the entries", async () => {
  const { seen, restore } = installFetch(() =>
    jsonResponse({
      data: [
        { type: "model", id: "claude-sonnet-4-5", display_name: "Claude Sonnet 4.5", created_at: "2025-09-29T00:00:00Z" },
        { id: "claude-haiku-4-5" },
      ],
      has_more: false,
      first_id: "claude-sonnet-4-5",
      last_id: "claude-haiku-4-5",
    }),
  );

  try {
    const models = await provider().listModels();
    const sent = seen[0];
    assert(sent);
    assert.equal(sent.url, "https://api.anthropic.com/v1/models");
    assert.equal(sent.method, undefined, "the catalog is a GET");
    assert.equal(sent.headers["x-api-key"], "sk-ant-test");
    assert.equal(sent.headers["anthropic-version"], "2023-06-01");

    assert.deepEqual(models, [
      { id: "claude-sonnet-4-5", label: "Claude Sonnet 4.5", contextWindow: undefined, supportsToolCalling: true, supportsStreaming: true },
      { id: "claude-haiku-4-5", label: "claude-haiku-4-5", contextWindow: undefined, supportsToolCalling: true, supportsStreaming: true },
    ]);
  } finally {
    restore();
  }
});

test("a malformed catalog fails loudly instead of looking like an empty model list", async () => {
  const { restore } = installFetch(() => jsonResponse({ items: [] }));

  try {
    await assert.rejects(provider().listModels(), (error: unknown) => {
      assert(error instanceof ProviderError);
      assert.equal(error.retryable, false);
      assert.match(error.message, /invalid model catalog/);
      return true;
    });
  } finally {
    restore();
  }
});

test("an HTTP failure on the catalog is retryable", async () => {
  const { restore } = installFetch(() => new Response("<html>bad gateway</html>", { status: 502 }));

  try {
    await assert.rejects(provider().listModels(), (error: unknown) => {
      assert(error instanceof ProviderError);
      assert.equal(error.retryable, true);
      assert.equal(error.status, 502);
      assert.doesNotMatch(error.message, /bad gateway/);
      return true;
    });
  } finally {
    restore();
  }
});
