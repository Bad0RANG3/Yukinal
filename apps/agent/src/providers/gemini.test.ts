/**
 * Gemini `generateContent` adapter tests. Fake `fetch` + constructed SSE bytes
 * only: nothing here touches the network, and nothing here asserts what the real
 * API does — only what this adapter puts on the wire and what it makes of the
 * bytes that come back.
 */

import assert from "node:assert/strict";
import test from "node:test";

import type { ChatRequest, StreamEvent } from "@yukinal/provider-sdk";
import { ProviderError } from "@yukinal/provider-sdk";

import { GeminiProvider } from "./gemini.js";

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

/** Gemini 的 SSE 只有 `data:` 行，一行一段完整的 JSON。 */
function sseChunks(chunks: JsonObject[]): string {
  return chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("");
}

function sseResponse(payload: string[], options: { close?: boolean; signal?: AbortSignal } = {}): Response {
  const { close = true, signal } = options;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of payload) controller.enqueue(encoder.encode(chunk));
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

function provider(overrides: Partial<ConstructorParameters<typeof GeminiProvider>[0]> = {}): GeminiProvider {
  return new GeminiProvider({
    baseUrl: "https://generativelanguage.googleapis.com",
    model: "gemini-2.5-flash",
    apiKey: "test-key",
    ...overrides,
  });
}

function chatRequest(overrides: Partial<ChatRequest> = {}): ChatRequest {
  return { model: "gemini-2.5-flash", messages: [{ role: "user", content: "看看 docker" }], ...overrides };
}

/** 跑完一次流：注入 fetch、收集全部事件、恢复全局 fetch。 */
async function collectStream(
  subject: GeminiProvider,
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

test("stream() sends generateContent: contents, systemInstruction, functionDeclarations, generationConfig", async () => {
  const request = chatRequest({
    messages: [
      { role: "system", content: "你是 Yukinal。" },
      { role: "user", content: "看看 docker" },
      { role: "assistant", content: "", toolCalls: [{ id: "gemini_call_1", name: "docker__ps", arguments: { all: true } }] },
      { role: "tool", toolCallId: "gemini_call_1", content: '{"containers":[{"id":"abc"}]}' },
      { role: "assistant", content: "有一个容器" },
    ],
    tools: [DOCKER_PS_TOOL],
  });

  const { seen } = await collectStream(provider(), request, () => sseResponse([sseChunks([{ candidates: [] }])]));
  const sent = seen[0];
  assert(sent, "the adapter must send exactly one request");

  assert.equal(
    sent.url,
    "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
  );
  assert.equal(sent.method, "POST");
  assert.equal(sent.headers["content-type"], "application/json");
  // 密钥走请求头而不是 `?key=`：URL 会进代理日志与错误上报，查询参数不该带着凭据。
  assert.equal(sent.headers["x-goog-api-key"], "test-key");
  assert.doesNotMatch(sent.url, /key=/);

  assert.deepEqual(sent.body.systemInstruction, { parts: [{ text: "你是 Yukinal。" }] });
  assert.deepEqual(sent.body.contents, [
    { role: "user", parts: [{ text: "看看 docker" }] },
    { role: "model", parts: [{ functionCall: { name: "docker__ps", args: { all: true } } }] },
    // 结果回填靠**函数名**（Gemini 没有调用 id）：id 是从上一轮 assistant 回合里反查出来的。
    { role: "user", parts: [{ functionResponse: { name: "docker__ps", response: { containers: [{ id: "abc" }] } } }] },
    { role: "model", parts: [{ text: "有一个容器" }] },
  ]);
  assert.deepEqual(sent.body.tools, [
    { functionDeclarations: [{ name: "docker__ps", description: "列出容器", parameters: DOCKER_PS_TOOL.function.parameters }] },
  ]);
  // 没给 maxOutputTokens 时不带这个键：硬塞默认值会把长回答静默截断。
  assert.deepEqual(sent.body.generationConfig, { temperature: 0 });
});

test("user images become inlineData parts", async () => {
  const request = chatRequest({
    messages: [
      {
        role: "user",
        content: "",
        images: [{ mediaType: "image/gif", data: "aGVsbG8=", name: "state.gif" }],
        documents: [
          { mediaType: "application/pdf", data: "JVBERi0xLjcK", name: "guide.pdf" },
        ],
        // Gemini 用同一个 inlineData 形状装每一种附件，音频也不例外：没有第二套词汇表。
        audios: [
          { mediaType: "audio/ogg", data: "T2dnUwAA", name: "note.ogg" },
          { mediaType: "audio/flac", data: "ZkxhQwAA" },
        ],
      },
    ],
  });

  const { seen } = await collectStream(provider(), request, () =>
    sseResponse([sseChunks([{ candidates: [] }])]),
  );
  const sent = seen[0];
  assert(sent);
  assert.deepEqual(sent.body.contents, [
    {
      role: "user",
      parts: [
        { inlineData: { mimeType: "image/gif", data: "aGVsbG8=" } },
        {
          inlineData: {
            mimeType: "application/pdf",
            data: "JVBERi0xLjcK",
          },
        },
        { inlineData: { mimeType: "audio/ogg", data: "T2dnUwAA" } },
        { inlineData: { mimeType: "audio/flac", data: "ZkxhQwAA" } },
      ],
    },
  ]);
});

test("request-level model, temperature and maxOutputTokens reach generationConfig", async () => {
  const subject = provider({ model: "gemini-2.5-flash" });
  const request = chatRequest({ model: "gemini-2.5-pro", temperature: 0.3, maxOutputTokens: 2048 });

  const { seen } = await collectStream(subject, request, () => sseResponse([sseChunks([{ candidates: [] }])]));
  const sent = seen[0];
  assert(sent);

  assert.match(sent.url, /\/models\/gemini-2\.5-pro:streamGenerateContent\?alt=sse$/);
  assert.deepEqual(sent.body.generationConfig, { temperature: 0.3, maxOutputTokens: 2048 });
});

test("a custom credential header cannot replace the configured key", async () => {
  const subject = provider({ customHeaders: { "X-Goog-Api-Key": "attacker", Authorization: "Bearer attacker", "X-Trace": "1" } });

  const { seen } = await collectStream(subject, chatRequest(), () => sseResponse([sseChunks([{ candidates: [] }])]));
  const sent = seen[0];
  assert(sent);

  assert.equal(sent.headers["x-goog-api-key"], "test-key");
  assert.equal(sent.headers["X-Goog-Api-Key"], undefined);
  assert.equal(sent.headers.Authorization, undefined);
  assert.equal(sent.headers["X-Trace"], "1", "non-credential custom headers survive");
});

test("an unresolvable toolCallId degrades to the id as the name, and text results are wrapped", async () => {
  const request = chatRequest({ messages: [{ role: "tool", toolCallId: "unknown_call", content: "plain text" }] });

  const { seen } = await collectStream(provider(), request, () => sseResponse([sseChunks([{ candidates: [] }])]));
  const sent = seen[0];
  assert(sent);

  assert.deepEqual(sent.body.contents, [
    { role: "user", parts: [{ functionResponse: { name: "unknown_call", response: { content: "plain text" } } }] },
  ]);
});

test("continuation text history is sent without a system role and without fragmenting a turn", async () => {
  const request = chatRequest({
    messages: [
      { role: "system", content: "s" },
      { role: "user", content: "两个都看看" },
      { role: "assistant", content: "好", toolCalls: [{ id: "gemini_call_1", name: "docker__ps", arguments: {} }] },
      { role: "tool", toolCallId: "gemini_call_1", content: "a" },
      { role: "tool", toolCallId: "gemini_call_2", content: "b" },
    ],
  });

  const { seen } = await collectStream(provider(), request, () => sseResponse([sseChunks([{ candidates: [] }])]));
  const sent = seen[0];
  assert(sent);

  assert.deepEqual(sent.body.contents, [
    { role: "user", parts: [{ text: "两个都看看" }] },
    { role: "model", parts: [{ text: "好" }, { functionCall: { name: "docker__ps", args: {} } }] },
    // 连续的结果合进同一轮：Gemini 的一轮内容只能有一个 role，分开等于伪造多轮对话。
    {
      role: "user",
      parts: [
        { functionResponse: { name: "docker__ps", response: { content: "a" } } },
        { functionResponse: { name: "gemini_call_2", response: { content: "b" } } },
      ],
    },
  ]);
  assert.equal((sent.body.contents as Array<{ role: string }>).some((content) => content.role === "system"), false);
});

test("a text stream emits text deltas, one usage event and a stop", async () => {
  const chunks = [
    sseChunks([
      { candidates: [{ content: { parts: [{ text: "你好" }] } }], usageMetadata: { promptTokenCount: 12, candidatesTokenCount: 3 } },
      { candidates: [{ content: { parts: [{ text: "，世界" }] } }], usageMetadata: { promptTokenCount: 12, candidatesTokenCount: 7 } },
      { candidates: [{ content: { parts: [] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 12, candidatesTokenCount: 7 } },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    { type: "text_delta", text: "你好" },
    { type: "text_delta", text: "，世界" },
    // usageMetadata 是累计值，每段都重报：只发最后一份，否则一次回复会发出三条重复用量。
    { type: "usage", inputTokens: 12, outputTokens: 7 },
    { type: "done", finishReason: "stop" },
  ]);
});

test("thought parts surface as reasoning_delta instead of leaking into the answer", async () => {
  const chunks = [
    sseChunks([
      { candidates: [{ content: { parts: [{ text: "我应该先看看容器状态。", thought: true }] } }] },
      { candidates: [{ content: { parts: [{ text: "有两个容器在跑。" }] }, finishReason: "STOP" }] },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    { type: "reasoning_delta", text: "我应该先看看容器状态。" },
    { type: "text_delta", text: "有两个容器在跑。" },
    { type: "done", finishReason: "stop" },
  ]);
});

test("functionCall parts become tool_call events with synthesized ids and verbatim names", async () => {
  const chunks = [
    sseChunks([
      {
        candidates: [
          {
            content: {
              parts: [
                { functionCall: { name: "docker__ps", args: { all: true } } },
                { functionCall: { name: "server__info", args: {} } },
              ],
            },
            finishReason: "STOP",
          },
        ],
        usageMetadata: { promptTokenCount: 20, candidatesTokenCount: 9 },
      },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    // Gemini 不返回调用 id，适配器按流内顺序合成；名字 `docker__ps` 原样通过。
    { type: "tool_call", call: { id: "gemini_call_1", name: "docker__ps", arguments: { all: true } } },
    { type: "tool_call", call: { id: "gemini_call_2", name: "server__info", arguments: {} } },
    { type: "usage", inputTokens: 20, outputTokens: 9 },
    { type: "done", finishReason: "stop" },
  ]);
});

test("finishReason maps onto FinishReason", async () => {
  const cases: Array<[string | null, string]> = [
    ["STOP", "stop"],
    ["MAX_TOKENS", "length"],
    ["SAFETY", "stop"],
    ["RECITATION", "stop"],
    ["SOMETHING_NEW", "stop"],
    [null, "stop"],
  ];

  for (const [reason, expected] of cases) {
    const { events } = await collectStream(provider(), chatRequest(), () =>
      sseResponse([sseChunks([{ candidates: [{ content: { parts: [] }, finishReason: reason }] }])]),
    );
    assert.deepEqual(events.at(-1), { type: "done", finishReason: expected }, `finishReason=${String(reason)}`);
  }
});

test("a nameless functionCall is dropped and unparseable args degrade to { raw }", async () => {
  const chunks = [
    sseChunks([
      {
        candidates: [
          {
            content: {
              parts: [
                { functionCall: { name: "", args: { all: true } } },
                // 经过网关重新序列化后 args 可能是字符串。
                { functionCall: { name: "docker__ps", args: '{"all":true}' } },
                { functionCall: { name: "shell__run", args: "not json" } },
              ],
            },
          },
        ],
      },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events, [
    { type: "tool_call", call: { id: "gemini_call_1", name: "docker__ps", arguments: { all: true } } },
    { type: "tool_call", call: { id: "gemini_call_2", name: "shell__run", arguments: { raw: "not json" } } },
    { type: "done", finishReason: "stop" },
  ]);
});

test("a promptFeedback refusal surfaces as a sanitized non-retryable error with no done", async () => {
  const chunks = [sseChunks([{ promptFeedback: { blockReason: "SAFETY" }, candidates: [] }])];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  const error = events.at(-1);
  assert(error && error.type === "error", "a refusal must not look like an empty answer");
  assert.equal(error.retryable, false, "resending the same prompt would be blocked again");
  assert.match(error.message, /blockReason: SAFETY/);
  assert.equal(events.some((event) => event.type === "done"), false);
});

test("a mid-stream error payload surfaces sanitized and non-retryable", async () => {
  const chunks = [
    sseChunks([
      { candidates: [{ content: { parts: [{ text: "部分回答" }] } }] },
      { error: { code: 500, status: "INTERNAL", message: "internal failure (api key: ****9z9z)" } },
    ]),
  ];

  const { events } = await collectStream(provider(), chatRequest(), () => sseResponse(chunks));

  assert.deepEqual(events[0], { type: "text_delta", text: "部分回答" });
  const error = events.at(-1);
  assert(error && error.type === "error");
  assert.equal(error.retryable, false);
  assert.match(error.message, /internal failure/);
  assert.doesNotMatch(error.message, /9z9z/);
  assert.match(error.message, /\[redacted\]/);
  assert.equal(events.some((event) => event.type === "done"), false);
});

test("cancellation aborts the in-flight request and ends as done(cancelled)", { timeout: 5_000 }, async () => {
  const controller = new AbortController();
  const { seen, restore } = installFetch((init) =>
    sseResponse(
      [sseChunks([{ candidates: [{ content: { parts: [{ text: "半句话" }] } }] }])],
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
    (init) => sseResponse([sseChunks([{ candidates: [{ content: { parts: [{ text: "半句" }] } }] }])], { close: false, signal: init?.signal ?? undefined }),
  );

  const error = events.at(-1);
  assert(error && error.type === "error", "a timeout is an error event, never a silent hang");
  assert.equal(error.retryable, false);
  assert.match(error.message, /timed out/);
});

test("an HTTP failure is a retryable ProviderError that never echoes the response body", async () => {
  const { restore } = installFetch(
    () => new Response('{"error":{"message":"API key not valid: ****g2z5"}}', { status: 500 }),
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

test("a 429 rate limit is retryable without echoing the response body", async () => {
  const { restore } = installFetch(
    () => new Response('{"error":{"message":"rate limit token ****g2z9"}}', { status: 429 }),
  );

  try {
    await assert.rejects(
      async () => {
        for await (const event of provider().stream(chatRequest())) assert.fail(`unexpected event ${event.type}`);
      },
      (error: unknown) => {
        assert(error instanceof ProviderError);
        assert.equal(error.retryable, true, "429 is retryable");
        assert.equal(error.status, 429);
        assert.match(error.message, /failed \(429\)/);
        assert.doesNotMatch(error.message, /g2z9/);
        return true;
      },
    );
  } finally {
    restore();
  }
});

test("listModels() keeps only generateContent models and strips the models/ prefix", async () => {
  const { seen, restore } = installFetch(() =>
    jsonResponse({
      models: [
        {
          name: "models/gemini-2.5-flash",
          displayName: "Gemini 2.5 Flash",
          inputTokenLimit: 1_048_576,
          supportedGenerationMethods: ["generateContent", "countTokens"],
        },
        { name: "models/gemini-embedding-001", displayName: "Gemini Embedding", supportedGenerationMethods: ["embedContent"] },
        { name: "models/gemini-2.5-pro", supportedGenerationMethods: ["generateContent"] },
        { name: "models/gemini-without-methods" },
      ],
    }),
  );

  try {
    const models = await provider().listModels();
    const sent = seen[0];
    assert(sent);
    assert.equal(sent.url, "https://generativelanguage.googleapis.com/v1beta/models");
    assert.equal(sent.method, undefined, "the catalog is a GET");
    assert.equal(sent.headers["x-goog-api-key"], "test-key");

    assert.deepEqual(models, [
      { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash", contextWindow: 1_048_576, supportsToolCalling: true, supportsStreaming: true },
      { id: "gemini-2.5-pro", label: "gemini-2.5-pro", contextWindow: undefined, supportsToolCalling: true, supportsStreaming: true },
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
