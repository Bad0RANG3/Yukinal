/**
 * 出站网络设置与状态（ADR 0022）。
 *
 * 代理**只改变连接怎么走**：endpoint 的 HTTPS 策略、重定向策略与认证头一条都不放宽；
 * 「读到了却用不了」的配置（只配了 PAC、URL 里内嵌凭据）不是被忽略，而是要在设置页上
 * 说明白为什么这次连接会失败。
 */

/** 用户选的模式。默认直连：装上代理软件不该悄悄改变应用的连接路径。 */
export type NetworkProxyMode = "direct" | "system";

/**
 * 此刻解析成什么。
 *
 * `kind` 是判别式：UI 按它分支，而不是猜字段在不在。`proxy` 的 `source` 回答
 * 「这条代理是谁定的」（环境变量 / Windows Internet 设置 / macOS 网络设置）。
 */
export type NetworkProxyResolution =
  | { kind: "direct" }
  | { kind: "proxy"; url: string; source: string; hasNoProxy: boolean }
  | { kind: "unusable"; reason: string };

export interface NetworkProxyView {
  mode: NetworkProxyMode;
  /** 凭据库里有没有代理凭据。值本身永远不出现在这里。 */
  hasCredential: boolean;
  resolution: NetworkProxyResolution;
}

/**
 * 保存入参。
 *
 * `credential` 是只写字段（`user:password`）：非空表示轮换，缺省或空表示保留已存的；
 * `clearCredential` 才是显式删除 —— 「留空保留」与「删掉」必须是两个动作。
 */
export interface NetworkProxySaveInput {
  mode: NetworkProxyMode;
  credential?: string;
  clearCredential?: boolean;
}
