/** 出站网络设置（ADR 0022）契约的形状，以及那些**必须被拒绝**的形状。 */

import assert from "node:assert/strict";
import test from "node:test";

import {
  NetworkProxySaveInputSchema,
  NetworkProxyViewSchema,
} from "./network.js";

test("every resolution variant the host can report parses", () => {
  for (const resolution of [
    { kind: "direct" },
    {
      kind: "proxy",
      url: "http://proxy.corp.example:3128",
      source: "环境变量",
      hasNoProxy: false,
    },
    { kind: "unusable", reason: "系统代理只配置了 PAC（http://wpad/wpad.dat）" },
  ]) {
    const parsed = NetworkProxyViewSchema.safeParse({
      mode: "system",
      hasCredential: false,
      resolution,
    });
    assert.equal(parsed.success, true, `${resolution.kind} must be a valid resolution`);
  }
});

test("an unknown resolution kind is refused instead of ignored", () => {
  // 判别式的作用就是这里：宿主报了一个我们不认识的形状时，界面必须失败，而不是
  // 显示成「直连」—— 那会让人以为请求没有经过代理。
  assert.equal(
    NetworkProxyViewSchema.safeParse({
      mode: "system",
      hasCredential: false,
      resolution: { kind: "socks5", url: "socks5://proxy:1080" },
    }).success,
    false,
  );
});

test("the save input carries a credential or an explicit clear, never both", () => {
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "system" }).success,
    true,
    "absent credential means `keep what is stored`",
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "system", credential: "user:pw" }).success,
    true,
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "system", credential: "user:pw", clearCredential: true }).success,
    false,
    "a new credential and an explicit clear are mutually exclusive",
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "direct", credential: "user:pw" }).success,
    false,
    "a new proxy credential is only meaningful in system-proxy mode",
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "direct", clearCredential: true }).success,
    true,
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "system", credential: "" }).success,
    false,
    "an empty credential is a UI bug: `keep` is the absent field, `delete` is clearCredential",
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "system", credential: "x".repeat(1_025) }).success,
    false,
    "the credential has a hard upper bound",
  );
  assert.equal(
    NetworkProxySaveInputSchema.safeParse({ mode: "proxy" }).success,
    false,
    "there are exactly two modes",
  );
});
