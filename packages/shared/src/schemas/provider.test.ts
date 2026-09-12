import assert from "node:assert/strict";
import test from "node:test";

import {
  AiProviderKindSchema,
  ProviderConfigSchema,
  ProviderSaveInputSchema,
  SafeCustomHeadersSchema,
} from "./provider.js";
import { RuntimeProviderConfigSchema } from "./permission.js";

test("provider custom headers accept metadata but reject credential channels", () => {
  assert.deepEqual(SafeCustomHeadersSchema.parse({ "HTTP-Referer": "https://desktop.example" }), {
    "HTTP-Referer": "https://desktop.example",
  });
  assert.equal(SafeCustomHeadersSchema.safeParse({ Authorization: "Bearer not-for-storage" }).success, false);
  assert.equal(SafeCustomHeadersSchema.safeParse({ "X-Api-Key": "not-for-storage" }).success, false);
});

test("custom provider IDs follow OpenCode's lowercase identifier format", () => {
  const valid = ProviderSaveInputSchema.safeParse({
    providerId: "my-provider_01",
    kind: "openai-compatible",
    label: "My Provider",
    baseUrl: "https://api.example.com/v1",
    model: "model-id",
  });
  assert.equal(valid.success, true);
  assert.equal(ProviderSaveInputSchema.safeParse({
    providerId: "My Provider",
    kind: "openai-compatible",
    label: "My Provider",
    baseUrl: "https://api.example.com/v1",
    model: "model-id",
  }).success, false);
});

/* ── kind 轴（ADR 0011） ───────────────────────────────────────────────────── */

test("the kind vocabulary is the three protocols, and nothing else parses", () => {
  for (const kind of ["openai-compatible", "anthropic", "gemini"]) {
    assert.equal(AiProviderKindSchema.safeParse(kind).success, true, kind);
  }
  // An unrecognised kind must not be coerced into the one kind that used to exist:
  // doing so would hand the agent an Anthropic or Gemini endpoint and speak OpenAI at it.
  for (const bogus of ["cohere", "openai", "openaicompatible", "", "ANTHROPIC"]) {
    assert.equal(AiProviderKindSchema.safeParse(bogus).success, false, bogus);
  }
});

test("provider_save requires a kind instead of defaulting to one", () => {
  assert.equal(
    ProviderSaveInputSchema.safeParse({
      baseUrl: "https://api.example.com/v1",
      model: "model-id",
    }).success,
    false,
    "a save without a kind would have to pick a protocol on the user's behalf",
  );
  assert.equal(
    ProviderSaveInputSchema.safeParse({
      kind: "gemini",
      baseUrl: "https://generativelanguage.googleapis.com",
      model: "gemini-2.5-flash",
    }).success,
    true,
  );
});

/*
 * `kind` 与 `wireApi` 正交，所以「gemini + wireApi: responses」既不是合法的 responses 配置，
 * 也不是一个可以静默忽略的字段。三个 schema 都要拒绝它 —— 保存路径、随运行下发的运行时配置，
 * 以及从数据库读回来的响应。
 */
test("wireApi is rejected wherever the kind has no dialect axis", () => {
  const save = (kind: string, wireApi?: string): boolean =>
    ProviderSaveInputSchema.safeParse({
      kind,
      baseUrl: "https://api.example.com",
      model: "m",
      ...(wireApi === undefined ? {} : { wireApi }),
    }).success;

  assert.equal(save("anthropic", "responses"), false);
  assert.equal(save("gemini", "chat"), false);
  assert.equal(save("anthropic"), true);
  assert.equal(save("gemini"), true);
  assert.equal(save("openai-compatible", "responses"), true);

  assert.equal(
    RuntimeProviderConfigSchema.safeParse({
      kind: "gemini",
      baseUrl: "https://generativelanguage.googleapis.com",
      model: "gemini-2.5-flash",
      wireApi: "responses",
    }).success,
    false,
    "the per-run config must not carry a dialect the kind cannot use",
  );
  assert.equal(
    RuntimeProviderConfigSchema.safeParse({
      kind: "anthropic",
      baseUrl: "https://api.anthropic.com",
      model: "claude-sonnet-4-5",
    }).success,
    true,
  );

  const row = {
    id: "prv_gemini",
    label: "Gemini",
    baseUrl: "https://generativelanguage.googleapis.com",
    model: "gemini-2.5-flash",
    enabled: true,
    createdAt: "2026-01-01T00:00:00.000Z",
    updatedAt: "2026-01-01T00:00:00.000Z",
  };
  assert.equal(ProviderConfigSchema.safeParse({ ...row, kind: "gemini", wireApi: "chat" }).success, false);
  assert.equal(ProviderConfigSchema.safeParse({ ...row, kind: "gemini" }).success, true);
  assert.equal(ProviderConfigSchema.safeParse({ ...row, kind: "cohere" }).success, false);
});
