/**
 * 「这一块暂时没有内容」的四种说法：加载中、读取失败、空、浏览器预览。
 *
 * 为什么要有这个文件：这四个状态在 8 个页面里被手抄了 **17 遍**，而每一遍都
 * 有一点点不一样 —— 不是设计上的不一样，是抄漏了的不一样：
 *
 * - `error-panel` 有两种结构：三处带 `.error-panel-icon` 的警告图标、三处不带。
 *   两者在横向 flex 里排出来的位置不同（带图标时按钮在正文 div 内、独占一行；
 *   不带时按钮与正文并排）。**本文件刻意把两种都保留下来**（`showIcon` 开关），
 *   因为把其中一种改成另一种是肉眼可见的变化，属于设计决定，不属于重构。
 *   这个不一致本身值得单独处理，但不该混在「不改变渲染」的提交里。
 * - 重试按钮的文案有三种：`重试`、`重试采集`、`重新读取`。已提升为 `retryLabel`
 *   参数，让每一处的措辞成为明确选择，而不是复制时顺手改的。
 * - 文案的写法也漂了：`正在读取服务状态` / `无法读取服务状态` 是一对（服务页），
 *   但动态页写的是 `正在读取动态` / `无法读取动态`。统一成 `正在读取X` / `无法读取X`
 *   之后，`LoadingPanel` 与 `ErrorPanel` 就常常成对出现，读起来是一件事的两个时刻。
 *
 * 这里不碰 ARIA，是刻意的：`ServerList.tsx:55` 的载入失败没有 `role="alert"`，而
 * 同一个文件 `:56` 的操作失败有。给「首屏就渲染出来的错误面板」加 `role="alert"`
 * 对读屏用户是行为变化（可能被立刻播报），该由无障碍那一轮决定，不该顺手加。
 *
 * 组件本身是纯展示：不读 store、不发 IPC，因此只依赖 `Icon`。
 */

import { Icon, type IconName } from "./Icon.js";

/** 首屏或切页时的「正在读取」。结构与 `.loading-panel` 的 CSS 一一对应。 */
export function LoadingPanel({ title, hint }: { title: string; hint: string }) {
  return (
    <div className="loading-panel">
      <div className="loading-spinner" />
      <strong>{title}</strong>
      <span>{hint}</span>
    </div>
  );
}

/**
 * 读取失败的整页替代。
 *
 * @param showIcon 是否在正文左侧带警告图标。两种结构在现有页面里各占一半，
 *   视觉结果不同，所以必须显式声明而不是由组件替你选。
 * @param retryLabel 重试按钮的文案。默认「重试」；需要说明重试的是什么时
 *   （比如 `重试采集`）由调用方指定。
 */
export function ErrorPanel({
  title,
  message,
  onRetry,
  retryLabel = "重试",
  showIcon = false,
}: {
  title: string;
  message: string;
  onRetry: () => void;
  retryLabel?: string;
  showIcon?: boolean;
}) {
  const retry = (
    <button type="button" className="button-secondary" onClick={onRetry}>
      {retryLabel}
    </button>
  );

  // 两种结构逐字保留改动前的样子：带图标时按钮在正文 div 之内（于是独占一行），
  // 不带图标时按钮是 `.error-panel` 的第二个 flex 子项（于是与正文并排）。
  // 这不是笔误，是现状；统一它属于设计决定，`style-snapshot.mjs` 会守住。
  if (showIcon) {
    return (
      <div className="error-panel">
        <div className="error-panel-icon"><Icon name="warning" size="md" /></div>
        <div>
          <strong>{title}</strong>
          <p>{message}</p>
          {retry}
        </div>
      </div>
    );
  }

  return (
    <div className="error-panel">
      <div>
        <strong>{title}</strong>
        <p>{message}</p>
      </div>
      {retry}
    </div>
  );
}

/**
 * 泛用空状态：可选的图标 + 标题 + 一句话。
 *
 * @param extraClass 页面级的补充类（`log-empty`、`project-empty`、`service-empty`），
 *   用于给特定页面微调留白。不传就是基础 `.empty-state.page-empty`。
 * @param body 允许是节点：「没有可展示的日志」那一处的正文是
 *   `response.message ?? "日志源没有返回内容。"`，取的是数据里的原话。
 */
export function EmptyPanel({
  icon,
  title,
  body,
  extraClass,
}: {
  icon?: IconName;
  title: string;
  body: React.ReactNode;
  extraClass?: string;
}) {
  return (
    <div className={extraClass ? `empty-state page-empty ${extraClass}` : "empty-state page-empty"}>
      {icon ? <Icon name={icon} size="xl" /> : null}
      <h2>{title}</h2>
      <p>{body}</p>
    </div>
  );
}

/**
 * 浏览器预览的占位。
 *
 * 标题永远是「浏览器预览」，所以不开放为参数：这四处说的是同一件事 ——
 * 原生能力只在 Tauri 壳里可用，而预览**不伪造**远端数据。每个页面的正文
 * 各自说明自己缺的是什么，那才是需要传进来的部分。
 */
export function PreviewEmpty({ icon, body }: { icon?: IconName; body: React.ReactNode }) {
  return <EmptyPanel icon={icon} title="浏览器预览" body={body} />;
}
