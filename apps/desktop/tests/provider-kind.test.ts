/**
 * 设置界面里 kind 轴的那几条规则（ADR 0011）。
 *
 * 这些是被**渲染**用的判断，所以它们必须是可测的纯函数：一个「原生协议也显示 Wire API」
 * 的界面不是崩溃，而是让用户配出一个自己都解释不了的 Provider。
 */

import assert from "node:assert/strict";
import test from "node:test";

import { AI_PROVIDER_KINDS, ProviderSaveInputSchema } from "@yukinal/shared";

import {
  PROVIDER_KIND_DEFAULT_BASE_URL,
  PROVIDER_KIND_ORDER,
  PROVIDER_KIND_LABEL,
  providerKindBaseUrlHint,
  providerKindUsesApiVersion,
  providerKindUsesWireApi,
  providerHeadersText,
  parseProviderHeaders,
  providerSavePayload,
} from "../src/lib/providers.js";

test("the UI offers exactly the kinds the contract has", () => {
  assert.deepEqual([...PROVIDER_KIND_ORDER], [...AI_PROVIDER_KINDS]);
  for (const kind of AI_PROVIDER_KINDS) {
    assert.ok(PROVIDER_KIND_LABEL[kind], `${kind} must have a label`);
    assert.ok(providerKindBaseUrlHint(kind), `${kind} must explain its base URL`);
  }
});

test("wireApi is offered for openai-compatible only", () => {
  assert.equal(providerKindUsesWireApi("openai-compatible"), true);
  assert.equal(providerKindUsesWireApi("anthropic"), false);
  assert.equal(providerKindUsesWireApi("gemini"), false);
});

test("apiVersion is offered for Anthropic only and never leaks into another kind", () => {
  assert.equal(providerKindUsesApiVersion("anthropic"), true);
  assert.equal(providerKindUsesApiVersion("openai-compatible"), false);
  assert.equal(providerKindUsesApiVersion("gemini"), false);

  const anthropic = providerSavePayload({
    kind: "anthropic",
    baseUrl: "https://api.anthropic.com",
    model: "claude-sonnet-4-5",
    apiVersion: " 2026-01-01 ",
  });
  assert.equal(anthropic.apiVersion, "2026-01-01");

  const gemini = providerSavePayload({
    kind: "gemini",
    baseUrl: "https://generativelanguage.googleapis.com",
    model: "gemini-2.5-flash",
    apiVersion: "2026-01-01",
  });
  assert.equal(Object.hasOwn(gemini, "apiVersion"), true);
  assert.equal(gemini.apiVersion, undefined);
});

test("custom headers round-trip through the text editor without losing values", () => {
  const text = providerHeadersText({
    "X-Title": "Yukinal",
    "HTTP-Referer": "https://desktop.example",
  });
  assert.equal(text, "HTTP-Referer: https://desktop.example\nX-Title: Yukinal");
  assert.deepEqual(parseProviderHeaders(text), {
    "HTTP-Referer": "https://desktop.example",
    "X-Title": "Yukinal",
  });
  assert.throws(() => parseProviderHeaders("missing colon"), /第 1 行/);
  assert.throws(() => parseProviderHeaders("X-Title: one\nx-title: two"), /重复/);
});

/** openai-compatible 没有默认端点：那个 kind 覆盖的端点不是我们的，编一个等于替用户选了服务商。 */
test("only the native protocols have a default base URL", () => {
  assert.equal(PROVIDER_KIND_DEFAULT_BASE_URL["openai-compatible"], null);
  assert.equal(PROVIDER_KIND_DEFAULT_BASE_URL.anthropic, "https://api.anthropic.com");
  assert.equal(PROVIDER_KIND_DEFAULT_BASE_URL.gemini, "https://generativelanguage.googleapis.com");
});

test("a saved draft always carries the kind, and never a wireApi the kind cannot use", () => {
  const saved = providerSavePayload({
    providerId: "prv_claude",
    kind: "anthropic",
    label: "Claude",
    baseUrl: " https://api.anthropic.com ",
    model: " claude-sonnet-4-5 ",
    apiKey: "sk-secret",
    customHeaders: { "anthropic-beta": "feature-2026-01-01" },
    // 界面在原生 kind 下根本不渲染这一格，但即使有值也不该被发出去。
    wireApi: "responses",
  });
  assert.equal(saved.kind, "anthropic");
  assert.equal(saved.baseUrl, "https://api.anthropic.com");
  assert.equal(saved.model, "claude-sonnet-4-5");
  assert.deepEqual(saved.customHeaders, { "anthropic-beta": "feature-2026-01-01" });
  assert.equal(Object.hasOwn(saved, "wireApi"), false, "a native kind must not carry wireApi");

  const compatible = providerSavePayload({
    kind: "openai-compatible",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "anthropic/claude-sonnet",
    wireApi: "responses",
  });
  assert.equal(compatible.wireApi, "responses");

  // 空串变成「不发送」：共享 schema 是 strictObject，空 label/apiKey 是 drift。
  // （键可能以 `undefined` 的形式存在 —— 序列化时不会出现在 payload 里，schema 也按缺省处理；
  //  真正必须**不存在**的只有 `wireApi`，上面那条断言钉的就是它。）
  const minimal = providerSavePayload({ kind: "gemini", baseUrl: "https://generativelanguage.googleapis.com", model: "gemini-2.5-flash", label: "", apiKey: "" });
  assert.equal(minimal.label, undefined);
  assert.equal(minimal.apiKey, undefined);
  assert.equal(minimal.models, undefined);
  assert.equal(minimal.providerId, undefined);

  // 最关键的一条：界面产出的 payload 必须真的能被那道 IPC 门接受，三种 kind 都是。
  for (const kind of AI_PROVIDER_KINDS) {
    const parsed = ProviderSaveInputSchema.safeParse(
      providerSavePayload({
        kind,
        baseUrl: PROVIDER_KIND_DEFAULT_BASE_URL[kind] ?? "https://gw.example.com/v1",
        model: "m",
        wireApi: "chat",
      }),
    );
    assert.equal(parsed.success, true, `the UI payload for ${kind} must pass the IPC gate`);
    assert.equal(parsed.data?.kind, kind);
  }
});
