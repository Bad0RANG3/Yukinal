/**
 * 出站网络设置的界面侧：读取当前状态、提交修改、以及把表单翻译成命令入参。
 *
 * 翻译放在这里而不是组件里，是因为它是**纯**函数：`credential` 只写、`clearCredential`
 * 才是删除，这两件事在界面上长得几乎一样（一个输入框留空 vs 一个勾选框），只有一条
 * 明确规则才能保证「留空 = 保留」不会在某次改动里变成「留空 = 删掉」。
 */

import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type NetworkProxyMode,
  type NetworkProxySaveInput,
  type NetworkProxyView,
} from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "./ipc.js";

export const NETWORK_PROXY_QUERY_KEY = ["network-proxy"] as const;

/** 表单在界面上持有的三个值。 */
export interface NetworkProxyForm {
  mode: NetworkProxyMode;
  /** 只写：空字符串表示「不轮换」，不是「删掉」。 */
  credential: string;
  /** 显式删除已存凭据。 */
  clearCredential: boolean;
}

export function useNetworkProxy() {
  return useQuery({
    queryKey: NETWORK_PROXY_QUERY_KEY,
    enabled: isDesktopShell(),
    queryFn: async () => await callDesktop(IPC_COMMANDS.networkProxyGet, {}),
  });
}

/**
 * 表单 → 保存入参。
 *
 * 两条规则：凭据只在非空时发送（空字符串不发送，因为「保留」是缺省字段而不是一个值），
 * 清除标记只在真的勾上时发送；两者同时出现是不可能的（勾选清除会禁用输入框）。
 */
export function networkProxyInput(form: NetworkProxyForm): NetworkProxySaveInput {
  const credential = form.credential.trim();
  return {
    mode: form.mode,
    ...(form.clearCredential
      ? { clearCredential: true }
      : form.mode === "system" && credential.length > 0
        ? { credential: form.credential }
        : {}),
  };
}

/** 表单的初值：从服务端读回的状态出发。 */
export function networkProxyForm(view: NetworkProxyView): NetworkProxyForm {
  return {
    mode: view.mode,
    // 凭据永远不回填：服务端也不会把它送回来。
    credential: "",
    clearCredential: false,
  };
}

/** 「这次请求会走哪里」那一行。 */
export function networkProxySummary(view: NetworkProxyView): string {
  switch (view.resolution.kind) {
    case "direct":
      return "当前：直连（不走任何代理）";
    case "proxy":
      return `当前：经 ${view.resolution.url}（来自${view.resolution.source}${
        view.resolution.hasNoProxy ? "，含 NO_PROXY 例外" : ""
      }）`;
    case "unusable":
      return `当前：不可用 —— ${view.resolution.reason}`;
  }
}

export async function saveNetworkProxy(form: NetworkProxyForm): Promise<NetworkProxyView> {
  return await callDesktop(IPC_COMMANDS.networkProxySave, { input: networkProxyInput(form) });
}
