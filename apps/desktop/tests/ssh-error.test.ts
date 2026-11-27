/**
 * SSH 失败码 → 界面建议。
 *
 * 这套断言真正在意的事只有一件：**不同的失败原因必须产生不同的下一步动作**。
 * 在此之前宿主把所有原因压成一个 `SSH_ERROR`，界面只能对所有情况说「检查地址、
 * 端口、用户名及认证信息」—— 那句话对密码被拒是对的、对指纹未核验是错的、
 * 对代理要认证更是无关。所以这里按「原因 → 建议」逐条对照，而不是只测「函数
 * 不抛异常」。
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  isSshFailure,
  sshFailureAdvice,
  sshFailureCode,
  stripFailureCode,
} from "../src/lib/ssh-error.js";

/** 宿主现在的形状：Error 带 code。 */
function hostError(code: string, message: string): Error {
  const error = new Error(`${message} (${code})`);
  return Object.assign(error, { code });
}

test("the host's code survives on the error object", () => {
  const error = hostError("SSH_AUTHENTICATION_FAILED", "SSH authentication failed");
  assert.equal(sshFailureCode(error), "SSH_AUTHENTICATION_FAILED");
  assert.equal(isSshFailure(error), true);
});

test("a code that only exists as a message suffix is still recognised", () => {
  // An older host process only appends the code to the sentence. The user may
  // keep running one until they restart, and the UI must not fall back to the
  // generic sentence just because the property is missing.
  const error = new Error("SSH authentication failed (SSH_AUTHENTICATION_FAILED)");
  assert.equal(sshFailureCode(error), "SSH_AUTHENTICATION_FAILED");
  assert.equal(isSshFailure(error), true);
});

test("non-SSH failures keep their own message and get no SSH advice", () => {
  const error = hostError("STORAGE_ERROR", "The local server database could not read the server");
  assert.equal(isSshFailure(error), false);
  assert.equal(sshFailureAdvice(error), undefined);

  const plain = new Error("输入校验失败：port：Number must be less than or equal to 65535");
  assert.equal(isSshFailure(plain), false);
  assert.equal(sshFailureAdvice(plain), undefined);
});

test("every cause the SSH core can report has its own next step", () => {
  const expected: Array<[string, RegExp]> = [
    ["SSH_AUTHENTICATION_FAILED", /重新填一遍密码/],
    ["SSH_CREDENTIAL_UNAVAILABLE", /重新填写认证信息/],
    ["SSH_HOST_KEY_UNVERIFIED", /探针/],
    ["SSH_TIMEOUT", /可达/],
    ["SSH_CONNECTION_FAILED", /端口/],
    ["SSH_DENIED_BY_POLICY", /白名单|黑名单/],
    ["SSH_RESOLVER_UNAVAILABLE", /DNS|IP/],
    ["SSH_RESOLVER_BUSY", /重试/],
    ["SSH_PROXY_TLS_FAILED", /代理/],
    ["SSH_PROXY_CONNECT_FAILED", /代理/],
    ["SSH_PROXY_AUTH_REQUIRED", /代理/],
    ["SSH_UNAVAILABLE", /SSH 工作进程|桌面应用/],
  ];
  for (const [code, hint] of expected) {
    const advice = sshFailureAdvice(hostError(code, "failed"));
    assert.ok(advice, `${code} must produce advice`);
    assert.equal(advice.code, code);
    assert.match(advice.hint, hint, `${code} must suggest the step that applies to it`);
  }
});

test("different causes never share the same sentence", () => {
  // The whole point: "check everything" was the only sentence before, and it was
  // wrong for most causes. Two causes that need different fixes must not render
  // identically.
  const first = sshFailureAdvice(hostError("SSH_AUTHENTICATION_FAILED", "x"));
  const second = sshFailureAdvice(hostError("SSH_HOST_KEY_UNVERIFIED", "x"));
  assert.ok(first && second);
  assert.notEqual(first.hint, second.hint);
  assert.notEqual(first.title, second.title);
});

test("an unknown SSH code falls back to the generic advice, not to a guess", () => {
  const advice = sshFailureAdvice(hostError("SSH_SOMETHING_NEW", "something new failed"));
  assert.ok(advice);
  assert.equal(advice.code, "SSH_SOMETHING_NEW");
  assert.match(advice.title, /SSH 连接失败/);
  // A cause we have no guidance for must not look like one we do.
  assert.doesNotMatch(advice.hint, /重新填一遍密码|探针|代理/);
});

test("the code suffix is stripped for display but the sentence is kept", () => {
  assert.equal(
    stripFailureCode("SSH authentication failed (SSH_AUTHENTICATION_FAILED)"),
    "SSH authentication failed",
  );
  assert.equal(stripFailureCode("连接被拒绝"), "连接被拒绝");
});
