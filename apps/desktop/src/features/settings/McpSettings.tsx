/**
 * MCP 服务器设置容器：查询、动作与提示文案。纯展示部分在 `McpSettingsPanel.tsx`，
 * 那里规定了「本地 MCP 进程怎么了」的每一句话怎么写。
 */

import { useEffect, useState } from "react";

import type { McpOAuthDeviceCodeEvent, McpServerView } from "@yukinal/shared";
import { errorMessage } from "../../lib/format.js";
import { openExternalUrl } from "../../lib/external.js";
import { subscribeDesktop } from "../../lib/ipc.js";
import {
  draftFromServer,
  emptyDraft,
  toSaveInput,
  useMcpServerActions,
  useMcpServers,
  type McpServerDraft,
} from "../../lib/mcp.js";
import {
  McpOAuthDeviceCodeDialog,
  type McpOAuthDeviceCodePhase,
} from "./McpOAuthDeviceCodeDialog.js";
import { McpSettingsPanel } from "./McpSettingsPanel.js";

export function McpSettings({ active = true }: { active?: boolean } = {}) {
  const serversQuery = useMcpServers({ enabled: active });
  const actions = useMcpServerActions();
  // 「正在编辑哪一行」是界面状态：`null` = 没有在编辑。它不是服务端状态，所以不进缓存。
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<McpServerDraft>(emptyDraft());
  const [notice, setNotice] = useState<string | null>(null);
  // 设备码流程：宿主在**等待期间**把 user_code 作为事件发出来，因为那个请求还没有返回。
  // 这里只保存「屏幕该显示什么」与用户自己的两个动作（复制、取消），没有令牌。
  const [devicePrompt, setDevicePrompt] = useState<McpOAuthDeviceCodeEvent | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [copyHint, setCopyHint] = useState<string | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);

  const servers = serversQuery.data?.servers ?? [];
  const error =
    (serversQuery.error ? errorMessage(serversQuery.error) : null) ??
    (actions.save.error ? errorMessage(actions.save.error) : null) ??
    (actions.remove.error ? errorMessage(actions.remove.error) : null) ??
    (actions.start.error ? errorMessage(actions.start.error) : null) ??
    (actions.stop.error ? errorMessage(actions.stop.error) : null) ??
    (actions.oauthCancel.error ? errorMessage(actions.oauthCancel.error) : null) ??
    // 设备码流程的终态由对话框说 —— 用户正看着那个代码；对话框关掉之后，这句话留在这里。
    (devicePrompt === null && actions.oauthConnect.error
      ? errorMessage(actions.oauthConnect.error)
      : null);

  useEffect(() => {
    const subscription = subscribeDesktop("mcp.oauth_device_code", (prompt) => {
      setDevicePrompt(prompt);
      setCancelled(false);
      setCopyHint(null);
      setLinkError(null);
    });
    return () => subscription.stop();
  }, []);

  const dismissDevicePrompt = (): void => {
    setDevicePrompt(null);
    setCancelled(false);
    setCopyHint(null);
    setLinkError(null);
  };

  const copyDeviceCode = (): void => {
    if (!devicePrompt) return;
    // 代码是给用户看的，不是机密：复制按钮只是省一次手抄。剪贴板不可用时如实说明，
    // 因为屏幕上的代码始终还在，用户仍然有办法。
    navigator.clipboard?.writeText(devicePrompt.userCode).then(
      () => setCopyHint("已复制到剪贴板。"),
      () => setCopyHint("这个环境不允许写剪贴板，请手动输入上面的代码。"),
    );
  };

  const openDeviceLink = (url: string): void => {
    setLinkError(null);
    void openExternalUrl(url).catch((cause: unknown) =>
      setLinkError(cause instanceof Error ? cause.message : String(cause)),
    );
  };

  const devicePhase: McpOAuthDeviceCodePhase = actions.oauthConnect.isPending
    ? "waiting"
    : cancelled
      ? "cancelled"
      : actions.oauthConnect.isError
        ? "failed"
        : "succeeded";
  const deviceMessage = cancelled
    ? "已取消这次授权；需要时重新点「连接 OAuth」会得到一个新的代码。"
    : linkError
      ? `无法打开链接：${linkError}`
      : actions.oauthConnect.error
        ? errorMessage(actions.oauthConnect.error)
        : null;

  const beginNew = (): void => {
    setEditing("");
    setDraft(emptyDraft());
    setNotice(null);
  };
  const beginEdit = (view: McpServerView): void => {
    setEditing(view.config.id);
    setDraft(draftFromServer(view.config));
    setNotice(null);
  };
  const submit = (): void => {
    actions.save.mutate(toSaveInput(draft), {
      onSuccess: (view) => {
        setEditing(null);
        setNotice(`已保存 ${view.config.label}（${view.config.id}）。保存不会启动它。`);
      },
    });
  };

  return (
    <>
      <McpSettingsPanel
        servers={servers}
        loading={serversQuery.isLoading}
        error={error}
        notice={notice}
        editingId={editing}
        draft={draft}
        busy={actions.busy}
        onDraftChange={setDraft}
        onBeginNew={beginNew}
        onBeginEdit={beginEdit}
        onCancelEdit={() => {
          setEditing(null);
          setNotice(null);
        }}
        onSubmit={submit}
        onStart={(serverId) =>
          actions.start.mutate(serverId, {
            onSuccess: (view) => {
              setNotice(
                view.status.running
                  ? `${view.config.label} 已启动（pid ${view.status.pid ?? "?"}），重启运行服务后生效。`
                  : `${view.config.label} 没有启动成功，原因见列表。`,
              );
            },
          })
        }
        onStop={(serverId) =>
          actions.stop.mutate(serverId, {
            onSuccess: (response) => {
              const shutdown = response.shutdown;
              setNotice(
                shutdown === undefined
                  ? `${response.server.config.label} 本来就没有在跑的进程。`
                  : shutdown.killed
                    ? `${response.server.config.label} 已关闭（关闭 stdin 无效，已强制结束）。`
                    : `${response.server.config.label} 已关闭。`,
              );
            },
          })
        }
        onDelete={(serverId) =>
          actions.remove.mutate(serverId, {
            onSuccess: (response) => {
              setNotice(
                response.stopped
                  ? `${serverId} 已删除，它的进程也一起关掉了。`
                  : `${serverId} 已删除（当时没有在跑的进程）。`,
              );
            },
          })
        }
        onReview={(input, label) =>
          actions.review.mutate(input, {
            onSuccess: () =>
              setNotice(
                `${label} 的工具审核已保存，重启运行服务后生效。`,
              ),
          })
        }
        onOAuthConnect={(serverId) => {
          // 新的一次授权不能沿用上一次留下的对话框状态（代码、取消标记、复制提示）。
          dismissDevicePrompt();
          actions.oauthConnect.mutate(serverId, {
            onSuccess: (response) =>
              setNotice(
                `${response.serverId} 已完成 OAuth 授权；运行中的旧会话已关闭，下次启动会使用新令牌。`,
              ),
          });
        }}
      />
      {devicePrompt ? (
        <McpOAuthDeviceCodeDialog
          prompt={devicePrompt}
          phase={devicePhase}
          message={deviceMessage}
          copyHint={copyHint}
          onOpenLink={openDeviceLink}
          onCopyCode={copyDeviceCode}
          onCancel={() => {
            // 界面先记住「是用户自己要停的」：后端随后报回来的那句失败是同一件事。
            setCancelled(true);
            setCopyHint(null);
            actions.oauthCancel.mutate(devicePrompt.serverId);
          }}
          onDismiss={dismissDevicePrompt}
        />
      ) : null}
    </>
  );
}

export { McpSettingsPanel } from "./McpSettingsPanel.js";
export type { McpSettingsPanelProps } from "./McpSettingsPanel.js";
