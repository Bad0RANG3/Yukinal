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

export function NetworkSettings({ active = true }: { active?: boolean } = {}) {
  const query = useNetworkProxy({ enabled: active });
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
  const dirty = Boolean(view && form && (
    form.mode !== view.mode || form.credential.trim() || form.clearCredential
  ));
  const updateDraft = (next: NetworkProxyForm) => {
    setDraft(next);
    mutation.reset();
  };

  return (
    <section className="settings-card">
      <div className="settings-card-header">
        <div>
          <h2>出站代理</h2>
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
          <fieldset className="settings-form-fields" disabled={mutation.isPending}>
          <div className="settings-form-grid">
            <label className="settings-field" htmlFor={field("mode")}>
              <span className="field-label">出站方式</span>
              <select
                id={field("mode")}
                className="form-input"
                value={form.mode}
                onChange={(event) =>
                  updateDraft({
                    ...form,
                    mode: event.target.value as NetworkProxyMode,
                  })
                }
              >
                <option value="direct">直连（默认）</option>
                <option value="system">系统代理</option>
              </select>
            </label>
            <label className="settings-field" htmlFor={field("credential")}>
              <span className="field-label">代理凭据（user:password）</span>
              <input
                id={field("credential")}
                className="form-input form-input-mono"
                type="password"
                autoComplete="off"
                spellCheck={false}
                disabled={form.mode !== "system" || form.clearCredential}
                value={form.credential}
                placeholder={view.hasCredential ? "留空保留已存凭据" : "可选"}
                onChange={(event) => updateDraft({ ...form, credential: event.target.value })}
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
                  updateDraft({ ...form, clearCredential: event.target.checked, credential: "" })
                }
              />
              <span>
                <strong>清除已存凭据</strong>
              </span>
            </label>
          </div>
          </fieldset>
          {dirty ? <p className="form-hint" role="status">有未保存的更改</p> : null}
          <div className="settings-card-actions">
            <button
              type="button"
              className="button-primary"
              disabled={mutation.isPending || !dirty}
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
          <span>已保存，下次连接生效</span>
        </div>
      ) : null}
    </section>
  );
}
