/**
 * 出站网络设置（[ADR 0022](../../../../docs/adr.md)）。
 *
 * 两件事必须一眼看得出来：
 *
 * 1. **默认是直连**，要经代理得自己选。装上代理软件不会悄悄改变应用的连接路径。
 * 2. **「此刻会走哪里」写在卡上**：环境变量、平台设置还是不可用（例如只配了 PAC）。
 *    只说「已启用系统代理」是不够的 —— 用户真正要问的是「那我现在的请求走哪里」。
 *
 * 代理凭据是只写字段：留空表示保留、勾选「清除」才是删除，两者在界面上长得像，所以
 * 翻译规则写在 `lib/network.ts` 里并被测试钉住。
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type NetworkProxyMode } from "@yukinal/shared";
import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import {
  NETWORK_PROXY_QUERY_KEY,
  networkProxyForm,
  networkProxySummary,
  saveNetworkProxy,
  useNetworkProxy,
  type NetworkProxyForm,
} from "../../lib/network.js";

export function NetworkSettings() {
  const query = useNetworkProxy();
  const queryClient = useQueryClient();
  const formId = useId();
  const field = (name: string) => `${formId}-${name}`;
  const [draft, setDraft] = useState<NetworkProxyForm | null>(null);
  const view = query.data;
  const form = draft ?? (view ? networkProxyForm(view) : null);

  const mutation = useMutation({
    mutationFn: saveNetworkProxy,
    onSuccess: (saved) => {
      queryClient.setQueryData(NETWORK_PROXY_QUERY_KEY, saved);
      setDraft(networkProxyForm(saved));
    },
  });

  return (
    <section className="settings-card">
      <div className="settings-card-header">
        <div>
          <p className="eyebrow">网络</p>
          <h2>出站代理</h2>
          <p>
            MCP 服务器、OAuth 授权与远端 KRL 下载共用这一份设置。代理只改变连接怎么走：
            endpoint 仍然必须是 HTTPS，重定向仍然不跟随。
          </p>
        </div>
        <Icon name="shield" size="lg" />
      </div>

      {query.isLoading ? (
        <p className="form-hint">正在读取网络设置…</p>
      ) : query.isError ? (
        <div className="settings-error-row" role="alert">
          <span>{query.error.message}</span>
          <button type="button" className="text-button" onClick={() => void query.refetch()}>
            重试
          </button>
        </div>
      ) : view && form ? (
        <>
          <p className="form-hint" role="status">
            {networkProxySummary(view)}
          </p>
          <div className="settings-form-grid">
            <label className="field-label" htmlFor={field("mode")}>
              出站方式
              <select
                id={field("mode")}
                className="form-input"
                value={form.mode}
                onChange={(event) =>
                  setDraft({
                    ...form,
                    mode: event.target.value as NetworkProxyMode,
                  })
                }
              >
                <option value="direct">直连（默认）</option>
                <option value="system">系统代理</option>
              </select>
            </label>
            <label className="field-label" htmlFor={field("credential")}>
              代理凭据（user:password）
              <input
                id={field("credential")}
                className="form-input form-input-mono"
                type="password"
                autoComplete="off"
                spellCheck={false}
                disabled={form.mode !== "system" || form.clearCredential}
                value={form.credential}
                placeholder={view.hasCredential ? "留空保留已存凭据" : "代理不需要认证时留空"}
                onChange={(event) => setDraft({ ...form, credential: event.target.value })}
              />
            </label>
          </div>
          <div className="settings-check-grid">
            <label className="settings-check-row">
              <input
                type="checkbox"
                disabled={!view.hasCredential}
                checked={form.clearCredential}
                onChange={(event) =>
                  setDraft({ ...form, clearCredential: event.target.checked, credential: "" })
                }
              />
              <span>
                <strong>清除已存凭据</strong>
                <small>
                  {view.hasCredential
                    ? "凭据只存在系统凭据库，界面不回填；删除后代理会按匿名请求发出去。"
                    : "当前没有已存凭据。"}
                </small>
              </span>
            </label>
            <label className="settings-check-row">
              <input type="checkbox" checked readOnly disabled />
              <span>
                <strong>代理凭据只进系统凭据库</strong>
                <small>SQLite、设置响应、日志与错误里都只有引用，没有值。</small>
              </span>
            </label>
          </div>
          <div className="settings-card-actions">
            <button
              type="button"
              className="button-primary"
              disabled={mutation.isPending}
              onClick={() => mutation.mutate(form)}
            >
              {mutation.isPending ? "保存中…" : "保存网络设置"}
            </button>
          </div>
        </>
      ) : null}

      {mutation.isError ? (
        <div className="settings-error-row" role="alert">
          <span>{mutation.error.message}</span>
        </div>
      ) : null}
      {mutation.isSuccess ? (
        <div className="settings-notice" role="status">
          <Icon name="shield" size="sm" />
          <span>已保存。修改对下一次连接生效（正在运行的 MCP 服务器需要重启）。</span>
        </div>
      ) : null}
    </section>
  );
}
