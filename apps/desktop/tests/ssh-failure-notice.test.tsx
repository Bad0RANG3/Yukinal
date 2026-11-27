/**
 * 连接失败的界面：不同的原因必须在界面上长得不一样。
 *
 * 这套断言盯着的是那次真实调试的结论。用户在服务器概览上反复看到
 * `SSH authentication failed (SSH_ERROR)`，唯一能做的建议是「检查地址、端口、
 * 用户名及认证信息」——而那五件事里只有一件是真的。查下来是宿主把 C++ SSH 核心
 * 的十二种 `error_code` 全折叠成 `SSH_ERROR`，码到 `ipc_router.cpp` 的
 * `return failure("SSH_ERROR", …)` 就没了。
 *
 * C++ 侧现在保留原因（`native/desktop_host/tests/test_ipc_router.cpp` 的
 * `check_ssh_failure_codes` 钉住映射），前端这一侧必须证明：拿到不同的码，
 * 渲染出来的句子确实不同，而且原始报文仍然可读。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { SshFailureNotice } from "../src/components/SshFailureNotice.js";

function hostError(code: string, message: string): Error {
  return Object.assign(new Error(`${message} (${code})`), { code });
}

test("a rejected password does not render like an unverified host key", () => {
  const rejected = renderToStaticMarkup(
    <SshFailureNotice error={hostError("SSH_AUTHENTICATION_FAILED", "SSH authentication failed")} />,
  );
  const unverified = renderToStaticMarkup(
    <SshFailureNotice error={hostError("SSH_HOST_KEY_UNVERIFIED", "Host key is not trusted yet")} />,
  );

  assert.notEqual(rejected, unverified, "two causes that need different fixes must not look the same");
  assert.match(rejected, /服务器拒绝了这次认证/);
  assert.match(rejected, /重新填一遍密码/);
  assert.doesNotMatch(rejected, /探针/);

  assert.match(unverified, /还没确认过这台服务器的主机指纹/);
  assert.match(unverified, /探针/);
  assert.doesNotMatch(unverified, /重新填一遍密码/);
});

test("the cause code is visible, and the core's own words stay reachable", () => {
  const html = renderToStaticMarkup(
    <SshFailureNotice error={hostError("SSH_AUTHENTICATION_FAILED", "SSH authentication failed")} />,
  );
  // The code is shown so it can be searched for or pasted into a report…
  assert.match(html, /<code>SSH_AUTHENTICATION_FAILED<\/code>/);
  // …while the core's sentence survives, folded rather than dropped.
  assert.match(html, /<details/);
  assert.match(html, /<summary>/);
  assert.match(html, /<pre>SSH authentication failed<\/pre>/);
  // The suffix would be a second copy of the code right next to it.
  assert.doesNotMatch(html, /SSH authentication failed \(SSH_AUTHENTICATION_FAILED\)/);
});

test("the notice is a live region, so a failed reconnect is announced", () => {
  const html = renderToStaticMarkup(
    <SshFailureNotice error={hostError("SSH_TIMEOUT", "SSH connection timed out")} />,
  );
  assert.match(html, /role="alert"/);
});

test("a failure the UI has no SSH advice for stays plain text", () => {
  // A storage error is not an SSH error: wearing the SSH advice would send the
  // user to re-enter a password that was never the problem.
  const html = renderToStaticMarkup(
    <SshFailureNotice
      error={hostError("STORAGE_ERROR", "The local server database could not read the server")}
    />,
  );
  assert.doesNotMatch(html, /ssh-failure/);
  assert.doesNotMatch(html, /重新填一遍密码|探针/);
  assert.match(html, /role="alert"/);
  assert.match(html, /The local server database could not read the server/);
});

test("a plain thrown string still renders as a sentence", () => {
  const html = renderToStaticMarkup(<SshFailureNotice error={"SSH authentication failed"} />);
  assert.match(html, /role="alert"/);
  // No code, no invented advice — the sentence the host gave is all there is.
  assert.doesNotMatch(html, /ssh-failure/);
  assert.match(html, /SSH authentication failed/);
});
