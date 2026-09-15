/**
 * 主机指纹面板（ADR 0012）：核验 / 钉住 / 遗忘。
 *
 * ## 这个文件为什么长这样
 *
 * 它被拆成一个容器（`HostKeySection`）和一个纯展示组件（`HostKeyPanel`）。容器负责
 * 查询与动作（`@tanstack/react-query` + IPC），展示组件只吃数据与回调 —— 于是「不一致时
 * 到底显示了什么」可以用 `renderToStaticMarkup` 直接钉住，而不需要先在测试里造出一个
 * Tauri 壳与一台 SSH 服务器。这个功能里最要紧的几件事（两个指纹都显示出来、没有「忽略」
 * 按钮、探针结果不叫「已验证」）恰恰都是**文案与结构**，不是数据。
 *
 * ## 四条不会让步的规则
 *
 * 1. **探针结果叫「服务器出示的指纹」，不叫「已验证」。** 探针只是真的连了一次并复述
 *    服务器的主张；在用户把它与自己手上的指纹核对之前，它什么都不能证明。按了探针
 *    就当核验过是真实存在的误用，只能靠文案缓解（ADR 0012 第 4 条）。
 * 2. **「信任此指纹」只在这一次会话真的探到过那个指纹之后才可用。** 规则在
 *    `lib/host-key.ts` 的 `hostKeyTrustEligibility` 里（纯函数，单独测）。
 * 3. **不一致时并列显示两个指纹**，并说清可能是什么 —— 服务器换了密钥，或者有人在中间。
 *    这里**没有**「忽略」「继续连接」「仍然信任」按钮：那不是少写一个按钮，而是这个功能
 *    的全部意义（第 5 条）。
 * 4. **「遗忘」的后果写在按钮旁边**：下一次连接会回到 TOFU，也就是不核验就自动接受。
 *    它不是一个无害的「清理」动作（ADR 0012 的 Consequences）。
 * 5. **显式 CA 策略优先于叶子 pin。** CA 模式连接只接受有效 host certificate，不会再查
 *    `known_hosts`。这时面板必须说清叶子 pin 不参与校验，并禁用只会造成误解的信任/遗忘。
 */

import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import type { ServerHostKeyProbeResult, ServerHostKeyStatus } from "@yukinal/shared";
import { errorMessage } from "../../lib/format.js";
import { useHostKeyActions, useHostKeyStatus, hostKeyTrustEligibility } from "../../lib/host-key.js";

/** 未核验时显示的占位词。不用空字符串：空字符串看起来像「指纹是空的」。 */
const UNPINNED_LABEL = "未核验";

const COMPARISON_NOTE: Record<ServerHostKeyProbeResult["comparison"], string> = {
  unpinned: "这台主机还没有被钉住。上面的指纹只是它这次自己说的。",
  matches: "服务器出示的指纹与已钉住的完全一致。",
  mismatch: "服务器出示的指纹与已钉住的不一致。",
};

export function HostKeySection({
  serverId,
  caPolicyEnabled = false,
}: {
  serverId: string;
  /** 已保存的服务器连接是否启用 CA/principal 校验（未保存的表单草稿不算）。 */
  caPolicyEnabled?: boolean;
}) {
  const statusQuery = useHostKeyStatus(serverId);
  const actions = useHostKeyActions(serverId);
  // 「这一次会话里探到的指纹」是界面状态，不是服务端状态：它就是「刚才我真的问过这台
  // 服务器」这件事本身，所以不放进 react-query 的缓存里（那会让它跨挂载存活）。
  const [probe, setProbe] = useState<ServerHostKeyProbeResult | null>(null);
  // 结果通知同理：它属于「刚刚那一下点了什么」，而不是这台服务器的状态。
  //
  // 不用 `actions.trust.isSuccess` 直接推：mutation 的成功标志会一直留在 hook 上，
  // 于是「已钉住这个指纹。」会在用户下一次探针之后还挂在那里，误导性地描述一个更早的动作。
  const [notice, setNotice] = useState<string | null>(null);

  const status = statusQuery.data;
  const error =
    (statusQuery.error ? errorMessage(statusQuery.error) : null) ??
    (actions.probe.error ? errorMessage(actions.probe.error) : null) ??
    (actions.trust.error ? errorMessage(actions.trust.error) : null) ??
    (actions.forget.error ? errorMessage(actions.forget.error) : null);

  return (
    <HostKeyPanel
      status={status}
      loading={statusQuery.isLoading}
      error={error}
      notice={notice}
      probe={probe}
      busy={actions.busy}
      probing={actions.probe.isPending}
      caPolicyEnabled={caPolicyEnabled}
      onProbe={() => {
        actions.probe.mutate(undefined, {
          onSuccess: (result) => {
            setProbe(result);
            setNotice(null);
          },
        });
      }}
      onTrust={() => {
        if (!probe) return;
        actions.trust.mutate(probe.presentedFingerprint, {
          // 钉子变了，这次的探针结果就不再描述现在 —— 丢掉它，按钮随之回到「需要先探针」。
          onSuccess: (result) => {
            setProbe(null);
            // 后端区分了「新建了钉子」与「本来就钉着这一个」：界面照实说，不把没做的事说成做了。
            setNotice(
              result.alreadyPinned
                ? `${result.fingerprint} 本来就钉着，没有重复写入。`
                : `已钉住 ${result.fingerprint}。今后这台主机出示别的指纹都会被拒绝。`,
            );
          },
        });
      }}
      onForget={() => {
        actions.forget.mutate(undefined, {
          // 同上：遗忘之后「探针时钉着的是什么」这句话已经不成立。
          onSuccess: (result) => {
            setProbe(null);
            setNotice(
              result.forgotten
                ? `已删除 ${result.host}:${result.port} 的钉子。下一次连接将回到首次连接的自动信任（TOFU），不再核验指纹。`
                : `${result.host}:${result.port} 本来就没有钉子，无需遗忘。`,
            );
          },
        });
      }}
    />
  );
}

export interface HostKeyPanelProps {
  status?: ServerHostKeyStatus;
  loading?: boolean;
  error?: string | null;
  notice?: string | null;
  /** 这一次会话里探到的结果；`null` = 还没探过。 */
  probe?: ServerHostKeyProbeResult | null;
  /** 有动作在跑（按钮一律禁用，避免边探边钉）。 */
  busy?: boolean;
  /** 正在探针 —— 与 `busy` 分开：按钮上的「探针中…」不能靠猜是哪一个动作在跑。 */
  probing?: boolean;
  /** 已保存的服务器连接启用了 CA/principal 校验；叶子指纹不再是信任依据。 */
  caPolicyEnabled?: boolean;
  onProbe: () => void;
  onTrust: () => void;
  onForget: () => void;
}

/** 纯展示：所有输入来自 props，可以直接渲染进静态 HTML 来断言。 */
export function HostKeyPanel({
  status,
  loading = false,
  error = null,
  notice = null,
  probe = null,
  busy = false,
  probing = false,
  caPolicyEnabled = false,
  onProbe,
  onTrust,
  onForget,
}: HostKeyPanelProps) {
  // 面板可能同时出现在多台服务器上（两个模态、或列表里的两行），所以标题 id 不能写死。
  const titleId = useId();
  const pinned = status?.pinnedFingerprint;
  const eligibility = hostKeyTrustEligibility(probe);
  // 摊平成 `null | reason`：`eligibility` 是判别联合，直接在两处 JSX 里访问 `.reason`
  // 需要每个位置都自己做收窄，摊平一次比在每个用法里写一次收窄更不容易出错。
  const trustBlockedBecause = eligibility.trustable ? null : eligibility.reason;
  const mismatch = probe?.comparison === "mismatch";
  const endpoint = status ? `${status.host}:${status.port}` : "";
  const trustDisabled = busy || caPolicyEnabled || !eligibility.trustable;
  const trustDisabledHint = caPolicyEnabled
    ? CA_POLICY_TRUST_HINT
    : trustBlockedBecause
      ? TRUST_DISABLED_HINT[trustBlockedBecause]
      : undefined;

  return (
    <section className="host-key-section" aria-labelledby={titleId}>
      <header className="host-key-header">
        <div>
          <p className="eyebrow">主机指纹</p>
          <h3 id={titleId}>核验服务器身份</h3>
        </div>
        {status ? <code className="host-key-endpoint">{endpoint}</code> : null}
      </header>

      {caPolicyEnabled ? (
        <div className="host-key-policy-note" role="note">
          <Icon name="shield" size="md" />
          <div>
            <strong>CA 策略已启用</strong>
            <p>
              此服务器连接只接受由已配置 CA 签发、且 principal 匹配的 host certificate。
              本机保存的叶子 host key 指纹及下面的比较结果不参与校验。
            </p>
          </div>
        </div>
      ) : null}

      {loading ? <p className="form-hint">正在读取本机记录的指纹…</p> : null}

      {status ? (
        <div className="host-key-current">
          <span className="host-key-label">
            {caPolicyEnabled ? "本机保存的叶子指纹" : "本机钉住的指纹"}
          </span>
          {pinned ? (
            <code className="host-key-fingerprint">{pinned}</code>
          ) : (
            <span className="host-key-unpinned">
              {caPolicyEnabled ? NO_SAVED_LEAF_LABEL : UNPINNED_LABEL}
            </span>
          )}
          <p className="form-hint">
            {caPolicyEnabled ? (
              <>
                这条 known_hosts 记录只是历史叶子指纹，当前不会影响连接。关闭并保存此服务器的
                CA 策略后，它才会重新成为 TOFU/匹配校验的钉子。
              </>
            ) : (
              <>
                指纹按 <strong>主机:端口</strong> 记录，不按服务器条目记录：同一台机器的另一个
                条目共用这条钉子。连接时以它为准，不一致即中断。
              </>
            )}
          </p>
        </div>
      ) : null}

      {probe ? (
        <div className={mismatch && !caPolicyEnabled ? "host-key-compare host-key-compare-mismatch" : "host-key-compare"}>
          <div className="host-key-fingerprint-item">
            <span className="host-key-label">
              {caPolicyEnabled ? "服务器出示的叶子指纹" : "服务器出示的指纹"}
            </span>
            <code className="host-key-fingerprint">{probe.presentedFingerprint}</code>
            {/* 这一行永远不会说「已验证」：出示不等于可信（ADR 0012 第 4 条）。 */}
            <p className="form-hint">
              {caPolicyEnabled
                ? "仅供查看；当前 CA 策略不使用它与本机叶子记录做信任判断。"
                : "这是它自己说的，尚未被核验。"}
            </p>
          </div>

          {!caPolicyEnabled && probe.pinnedFingerprint ? (
            <div className="host-key-fingerprint-item">
              <span className="host-key-label">本机钉住的指纹</span>
              <code className="host-key-fingerprint">{probe.pinnedFingerprint}</code>
            </div>
          ) : null}

          {/* 这一块本身就叫「不一致」，所以不再单独加 `role="alert"`：下面那段黄色警告
              已经是一次完整播报，两处都 alert 会让读屏用户听到两遍。 */}
          {!caPolicyEnabled ? (
            <p className={mismatch ? "form-error" : "form-hint"}>
              {COMPARISON_NOTE[probe.comparison]}
            </p>
          ) : null}

          {mismatch && !caPolicyEnabled ? (
            <div className="host-key-warning" role="alert">
              <Icon name="warning" size="md" />
              <p>
                服务器出示的密钥与这台主机上一次核验过的<strong>不是同一把</strong>。可能是服务器
                更换了密钥（正当的运维动作），也可能是有人正在拦截这次连接。这一步无法由 Yukinal
                判断 —— 请通过另一条渠道（服务器运维、控制台、上一次的备份记录）确认现在
                这把指纹确实是它。确认之后，先「遗忘」旧钉子，再重新探针并点「信任此指纹」。
                <br />
                在你确认之前，任何连接都会继续被拒绝。Yukinal 不提供「忽略」或「仍然继续」。
              </p>
            </div>
          ) : null}
        </div>
      ) : null}

      {error ? <p className="form-error" role="alert">{error}</p> : null}
      {notice ? <p className="form-success" role="status">{notice}</p> : null}

      <div className="host-key-actions">
        <button type="button" className="button-secondary" onClick={onProbe} disabled={busy || loading}>
          <Icon name="refresh" size="sm" /> {probing ? "探针中…" : "探针"}
        </button>
        <button
          type="button"
          className="button-primary"
          onClick={onTrust}
          disabled={trustDisabled}
          title={trustDisabledHint}
        >
          信任此指纹
        </button>
        {pinned ? (
          <button
            type="button"
            className="button-secondary host-key-forget"
            onClick={onForget}
            disabled={busy || caPolicyEnabled}
            title={caPolicyEnabled ? CA_POLICY_FORGET_HINT : undefined}
          >
            <Icon name="trash" size="sm" /> 遗忘
          </button>
        ) : null}
      </div>

      {caPolicyEnabled ? (
        <p className="form-hint">
          「探针」仍可查看服务器这次出示的叶子 host key，但连接是否可信由已配置的 CA 公钥、
          host certificate 有效期与 principal 决定。叶子指纹的信任或遗忘要等关闭并保存 CA
          策略后才能操作。
        </p>
      ) : (
        <p className="form-hint">
          「探针」会真的连接一次 {endpoint || "这台服务器"}：只读取它出示的指纹，不发送任何凭据、
          也不记录任何东西。核对指纹这一步必须由你完成 —— 在服务器上执行
          <code> ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub </code>
          （路径随发行版与密钥类型而变，通常是 <code>/etc/ssh/ssh_host_*_key.pub</code>），
          输出里 <code>SHA256:</code> 开头的那一串应当与上面完全一致。
        </p>
      )}

      {!caPolicyEnabled && eligibility.trustable ? (
        <p className="form-hint form-hint-warning">
          确认无误后再点「信任此指纹」。一旦钉住，今后这台主机出示别的指纹都会被拒绝。
        </p>
      ) : null}

      {!caPolicyEnabled && trustBlockedBecause === "mismatch" ? (
        <p className="form-hint form-hint-warning">
          不一致时不能直接信任新指纹：请先「遗忘」旧钉子，再重新探针确认。
        </p>
      ) : null}

      {!caPolicyEnabled && pinned ? (
        <p className="form-hint form-hint-warning">
          「遗忘」会删除这条钉子。<strong>下一次连接这台主机将不再核验</strong>，直接按首次连接
          信任并记录（TOFU）；已经建立的连接不受影响。要改钉子只能走这条路：遗忘 → 探针 → 信任。
        </p>
      ) : null}
    </section>
  );
}

const TRUST_DISABLED_HINT: Record<"no-probe" | "mismatch" | "already-pinned", string> = {
  "no-probe": "先点「探针」，拿到这台服务器这次出示的指纹之后才能钉住它。",
  mismatch: "出示的指纹与已钉住的不同：先「遗忘」旧钉子，再重新探针确认。",
  "already-pinned": "这个指纹已经钉住了，无需重复。",
};

const NO_SAVED_LEAF_LABEL = "未保存叶子指纹";
const CA_POLICY_TRUST_HINT = "CA 策略启用时，叶子指纹不参与连接校验。";
const CA_POLICY_FORGET_HINT = "先关闭并保存 CA 策略，才能修改叶子指纹钉子。";
