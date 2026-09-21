/**
 * 主机指纹闸门的放行规则。
 *
 * 这里只测**纯函数** `hostKeyGateDecision`，不渲染组件：这条规则决定「放行还是拦下」，
 * 是这里唯一会出安全问题的地方，而把它留在组件里写成一串 `&&` 就只能靠一个带 Tauri 壳
 * 的渲染测试去碰 —— 那恰好是最难、也最容易为错的理由变绿的地方。渲染层（loading /
 * error / gate / children）只是这个判断的四个分支。
 */

import assert from "node:assert/strict";
import test from "node:test";

import { hostKeyGateDecision } from "../src/features/servers/HostKeyGate.js";

/** 一个「什么都不满足」的基线：非 CA、未钉、状态已读到。 */
const base = {
  serverId: "srv_1",
  desktopShell: true,
  serversLoading: false,
  statusLoading: false,
  statusFailed: false,
  caPolicyEnabled: false,
  pinned: false,
};

test("an unpinned host is gated until the fingerprint is trusted", () => {
  assert.equal(hostKeyGateDecision(base), "gate");
  assert.equal(
    hostKeyGateDecision({ ...base, pinned: true }),
    "children",
    "a pin is what the gate exists to require",
  );
});

test("an explicit CA policy does not require a leaf pin", () => {
  // CA 模式下叶子 pin 不参与校验，拦下只会让人以为必须先钉一个无效的值。
  assert.equal(hostKeyGateDecision({ ...base, caPolicyEnabled: true }), "children");
  assert.equal(
    hostKeyGateDecision({ ...base, caPolicyEnabled: true, pinned: true }),
    "children",
  );
});

test("the gate never opens while the status is unknown or unreadable", () => {
  assert.equal(hostKeyGateDecision({ ...base, statusLoading: true }), "loading");
  assert.equal(hostKeyGateDecision({ ...base, serversLoading: true }), "loading");
  assert.equal(hostKeyGateDecision({ ...base, statusFailed: true }), "error");
  // fail closed：读不到状态时即便缓存里好像钉着，也不能放行建立连接。
  assert.equal(
    hostKeyGateDecision({ ...base, statusFailed: true, pinned: true }),
    "error",
  );
});

test("without a target, or outside the desktop shell, the panel decides for itself", () => {
  // 没有选服务器，或浏览器预览（没有原生连接能力）时，这个闸门不该替面板显示空状态。
  assert.equal(hostKeyGateDecision({ ...base, serverId: null }), "children");
  assert.equal(hostKeyGateDecision({ ...base, desktopShell: false }), "children");
  assert.equal(hostKeyGateDecision({ ...base, serverId: null, pinned: true }), "children");
});
