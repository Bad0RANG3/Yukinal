import type { AiProviderConfig } from "@yukinal/shared";

import { providerOptionLabel } from "../../lib/providers.js";

/**
 * 选择框里「新建」那一项的取值。
 *
 * 空串而不是 `null`：`<select>` 的 value 只能是字符串，而 Provider id 的 schema 不允许
 * 空串（`[a-z0-9][a-z0-9-_]*`），所以空串在这个控件里没有第二个含义。
 */
export const NEW_PROVIDER_VALUE = "";

/** 与列表里的徽章同一个词：只有一份「当前使用」的说法。 */
const CURRENT_BADGE = "当前使用";

export interface ProviderPickerProps {
  providers: AiProviderConfig[];
  /** 正在查看的那一个（下方表单正在编辑它）；`NEW_PROVIDER_VALUE` 表示在新建。 */
  viewingId: string;
  busy: boolean;
  /** 正在等二次确认的那一个；`null` 表示没有。由调用方持有，好让确认也能被静态渲染测到。 */
  confirmingId: string | null;
  onView: (providerId: string) => void;
  onActivate: (providerId: string) => void;
  onRequestDelete: (providerId: string) => void;
  onCancelDelete: () => void;
  onConfirmDelete: () => void;
}

/**
 * 选 Provider：一个选择框、一个动作，加一个删除。
 *
 * 这里原来把每个 Provider 都铺成一行 —— 每行带名称、模型、协议、wireApi、密钥状态和
 * 两个按钮。十四个 Provider 就是十四行，而用户此刻真正要回答的只有两个问题：**我在看
 * 哪一个**、**新建运行会用哪一个**。其余字段下面那张表单本来就摆着（协议、地址、模型、
 * 密钥状态），再说一遍只会互相打架。
 *
 * 因此：选择框负责「在看哪一个」，按钮负责「用哪一个」，下面一行只讲后果。切选择框**不会**
 * 顺手切换启用的 Provider —— 那会让「换一个看看」变成一次悄悄改了运行配置的操作。
 *
 * 删除走行内二次确认（与对话记录一致），并且**说清两件无法从别处看出来的后果**：密钥是否
 * 会被一并删掉（引用可以共享，所以要看还有没有别人在用），以及当前项被删后由谁接替。
 */
export function ProviderPicker({
  providers,
  viewingId,
  busy,
  confirmingId,
  onView,
  onActivate,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete,
}: ProviderPickerProps) {
  const viewing = providers.find((provider) => provider.id === viewingId);
  const confirming = viewing !== undefined && confirmingId === viewing.id;
  return (
    <div className="provider-picker">
      <div className="provider-picker-row">
        <label className="settings-field provider-picker-field">
          <span className="field-label">Provider</span>
          <select
            className="form-input"
            value={viewing ? viewing.id : NEW_PROVIDER_VALUE}
            disabled={busy}
            onChange={(event) => onView(event.target.value)}
          >
            {providers.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {providerOptionLabel(provider, providers)}
                {provider.enabled ? ` · ${CURRENT_BADGE}` : ""}
              </option>
            ))}
            <option value={NEW_PROVIDER_VALUE}>新建 Provider…</option>
          </select>
        </label>
        {viewing && !confirming ? (
          <div className="provider-picker-actions">
            <button
              type="button"
              className="button-secondary"
              disabled={busy || viewing.enabled}
              title={
                viewing.enabled
                  ? "新建的 Agent 运行已经在用它。"
                  : "把它设为新建 Agent 运行使用的 Provider；其余 Provider 会被停用（配置仍在）。"
              }
              onClick={() => onActivate(viewing.id)}
            >
              {viewing.enabled ? "当前使用中" : "设为当前"}
            </button>
            <button
              type="button"
              className="button-secondary provider-picker-delete"
              disabled={busy}
              onClick={() => onRequestDelete(viewing.id)}
            >
              删除
            </button>
          </div>
        ) : null}
      </div>
      {viewing && confirming ? (
        <div
          className="provider-picker-confirm"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.stopPropagation();
              onCancelDelete();
            }
          }}
        >
          <p>
            删除「{viewing.label}」？这份配置会消失
            {/* 没有密钥引用的行（本地端点、免鉴权网关）不该听到「密钥也会被删」—— 它没有密钥。 */}
            {viewing.apiKeyCredentialRef ? "；没有别的 Provider 引用同一份密钥时，密钥也会从系统密钥链移除。" : "。"}
            {viewing.enabled ? "它当前是新建运行使用的 Provider，删除后会自动改用列表里最近更新的那个。" : ""}
          </p>
          <div className="provider-picker-confirm-actions">
            {/* 默认焦点在「取消」上：回车和 Escape 都不该删掉东西。 */}
            <button type="button" className="text-button" autoFocus onClick={onCancelDelete}>
              取消
            </button>
            <button
              type="button"
              className="button-secondary provider-picker-confirm-delete"
              disabled={busy}
              onClick={onConfirmDelete}
            >
              {busy ? "删除中…" : "删除"}
            </button>
          </div>
        </div>
      ) : null}
      {!confirming ? (
        <p className="provider-picker-status">
          <span
            className={`provider-status-dot ${viewing?.enabled ? "provider-status-on" : "provider-status-off"}`}
          />
          <span>
            {!viewing
              ? "保存后会启用它，其余 Provider 自动停用。"
              : viewing.enabled
                ? "新建 Agent 运行会使用它。"
                : "未启用：配置保留，但新建 Agent 运行不会用到它。"}
          </span>
        </p>
      ) : null}
    </div>
  );
}
