import assert from "node:assert/strict";
import test from "node:test";

import type { ChatRequest, LLMProvider, ProviderToolSpec, StreamEvent } from "@yukinal/provider-sdk";

import { AnthropicProvider } from "../src/providers/anthropic.js";
import { GeminiProvider } from "../src/providers/gemini.js";

type LiveProviderName = "anthropic" | "gemini";

interface LiveProviderConfig {
  name: LiveProviderName;
  model: string;
  apiKey: string;
  baseUrl: string;
  apiVersion?: string;
}

const LIVE_FLAG = "YUKINAL_LIVE_PROVIDER_TESTS";
const LIVE_PROVIDERS = "YUKINAL_LIVE_PROVIDERS";

const liveEnabled = process.env[LIVE_FLAG] === "1";
const selectedProviders = new Set(
  (process.env[LIVE_PROVIDERS] ?? "")
    .split(",")
    .map((value) => value.trim().toLowerCase())
    .filter(Boolean),
);

const allProviderNames = new Set<LiveProviderName>(["anthropic", "gemini"]);
const unknownProviders = [...selectedProviders].filter(
  (provider): provider is string => !allProviderNames.has(provider as LiveProviderName),
);

if (liveEnabled && selectedProviders.size === 0) {
  throw new Error(`set ${LIVE_PROVIDERS}=anthropic,gemini to select live providers`);
}
if (unknownProviders.length > 0) {
  throw new Error(`unknown live providers: ${unknownProviders.join(",")}`);
}

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`missing ${name} for explicitly enabled live tests`);
  return value;
}

function configFor(name: LiveProviderName): LiveProviderConfig {
  if (name === "anthropic") {
    return {
      name,
      apiKey: requiredEnv("ANTHROPIC_API_KEY"),
      model: requiredEnv("YUKINAL_ANTHROPIC_MODEL"),
      baseUrl: process.env.YUKINAL_ANTHROPIC_BASE_URL?.trim() || "https://api.anthropic.com",
      apiVersion: process.env.YUKINAL_ANTHROPIC_API_VERSION?.trim() || undefined,
    };
  }
  return {
    name,
    apiKey: requiredEnv("GEMINI_API_KEY"),
    model: requiredEnv("YUKINAL_GEMINI_MODEL"),
    baseUrl:
      process.env.YUKINAL_GEMINI_BASE_URL?.trim() || "https://generativelanguage.googleapis.com",
  };
}

const configs = new Map<LiveProviderName, LiveProviderConfig>();
for (const name of liveEnabled ? selectedProviders : []) {
  if (allProviderNames.has(name as LiveProviderName)) configs.set(name as LiveProviderName, configFor(name as LiveProviderName));
}

function providerFor(config: LiveProviderConfig): LLMProvider {
  if (config.name === "anthropic") {
    return new AnthropicProvider({
      baseUrl: config.baseUrl,
      model: config.model,
      apiKey: config.apiKey,
      apiVersion: config.apiVersion,
      timeoutMs: 90_000,
    });
  }
  return new GeminiProvider({
    baseUrl: config.baseUrl,
    model: config.model,
    apiKey: config.apiKey,
    timeoutMs: 90_000,
  });
}

const echoTool: ProviderToolSpec = {
  type: "function",
  function: {
    name: "live__echo",
    description: "Return the supplied message unchanged.",
    parameters: {
      type: "object",
      properties: { message: { type: "string" } },
      required: ["message"],
      additionalProperties: false,
    },
  },
};

const imageData =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const pdfData =
  "JVBERi0xLjEKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAxMDAgMTAwXSA+PgplbmRvYmoKdHJhaWxlcgo8PCAvUm9vdCAxIDAgUiA+PgolJUVPRgo=";

function request(config: LiveProviderConfig, messages: ChatRequest["messages"], extra: Partial<ChatRequest> = {}): ChatRequest {
  return {
    model: config.model,
    messages,
    temperature: 0,
    maxOutputTokens: 256,
    timeoutMs: 90_000,
    ...extra,
  };
}

async function collect(provider: LLMProvider, requestToSend: ChatRequest): Promise<StreamEvent[]> {
  const events: StreamEvent[] = [];
  for await (const event of provider.stream(requestToSend)) {
    if (event.type === "error") throw new Error("live provider emitted a stream error");
    events.push(event);
  }
  return events;
}

function assertCompleted(events: StreamEvent[], label: string): void {
  assert.ok(events.some((event) => event.type === "text_delta"), `${label} must return text`);
  const done = events.findLast((event) => event.type === "done");
  assert.ok(done && done.type === "done", `${label} must finish with done`);
  assert.notEqual(done.finishReason, "error", `${label} must not finish with an error`);
}

function liveOptions(name: LiveProviderName): { skip: string | false } {
  if (!liveEnabled) return { skip: `set ${LIVE_FLAG}=1 to enable network tests` };
  if (!selectedProviders.has(name)) return { skip: `add ${name} to ${LIVE_PROVIDERS}` };
  return { skip: false };
}

function config(name: LiveProviderName): LiveProviderConfig {
  const value = configs.get(name);
  assert.ok(value, `missing live configuration for ${name}`);
  return value;
}

for (const name of ["anthropic", "gemini"] as const) {
  test(`${name} live text streaming and usage`, liveOptions(name), async () => {
    const current = config(name);
    const provider = providerFor(current);
    const events = await collect(
      provider,
      request(current, [
        { role: "system", content: "Answer briefly and plainly." },
        { role: "user", content: "Reply with the single word: ready" },
      ]),
    );
    assertCompleted(events, `${name} text stream`);
    assert.ok(events.some((event) => event.type === "usage"), `${name} must report usage`);
  });

  test(`${name} live tool call and tool result round trip`, liveOptions(name), async () => {
    const current = config(name);
    const provider = providerFor(current);
    const first = await collect(
      provider,
      request(
        current,
        [{ role: "user", content: "Call live__echo exactly once with message equal to ready. Do not answer until you call it." }],
        { tools: [echoTool] },
      ),
    );
    const callEvent = first.find((event): event is Extract<StreamEvent, { type: "tool_call" }> => event.type === "tool_call");
    assert.ok(callEvent, `${name} must produce a tool call`);
    assert.equal(callEvent.call.name, "live__echo");

    const second = await collect(
      provider,
      request(
        current,
        [
          { role: "user", content: "Call live__echo exactly once with message equal to ready." },
          { role: "assistant", content: "", toolCalls: [callEvent.call] },
          { role: "tool", toolCallId: callEvent.call.id, content: JSON.stringify({ message: "ready" }) },
        ],
        { tools: [echoTool] },
      ),
    );
    assertCompleted(second, `${name} tool result continuation`);
  });

  test(`${name} live cancellation after streamed text`, liveOptions(name), async () => {
    const current = config(name);
    const provider = providerFor(current);
    const controller = new AbortController();
    const events: StreamEvent[] = [];
    let aborted = false;
    for await (const event of provider.stream(
      request(
        current,
        [{ role: "user", content: "Write a detailed explanation of why streaming cancellation matters, using at least 200 words." }],
        { signal: controller.signal, maxOutputTokens: 512 },
      ),
    )) {
      if (event.type === "error") throw new Error(`${name} live cancellation emitted a stream error`);
      events.push(event);
      if (!aborted && event.type === "text_delta") {
        aborted = true;
        controller.abort();
      }
    }
    assert.ok(aborted, `${name} must emit text before cancellation`);
    const done = events.findLast((event) => event.type === "done");
    assert.ok(done && done.type === "done", `${name} cancellation must finish with done`);
    assert.equal(done.finishReason, "cancelled", `${name} cancellation must be observable`);
  });

  test(`${name} live image and PDF input`, liveOptions(name), async () => {
    const current = config(name);
    const provider = providerFor(current);
    const events = await collect(
      provider,
      request(current, [
        {
          role: "user",
          content: "Say ready if you received both the attached one-pixel image and the attached one-page PDF.",
          images: [{ mediaType: "image/png", data: imageData, name: "pixel.png" }],
          documents: [{ mediaType: "application/pdf", data: pdfData, name: "one-page.pdf" }],
        },
      ]),
    );
    assertCompleted(events, `${name} media request`);
  });
}

if (liveEnabled && selectedProviders.size > 0) {
  test("live run configuration is recorded without exposing credentials", () => {
    const recorded = [...configs.values()].map((current) => ({
      provider: current.name,
      model: current.model,
      baseUrl: current.baseUrl,
      apiVersion: current.apiVersion ?? "adapter default",
      date: new Date().toISOString().slice(0, 10),
    }));
    assert.ok(recorded.every((entry) => entry.model && entry.baseUrl));
    console.log(JSON.stringify({ live: recorded }));
  });
}
