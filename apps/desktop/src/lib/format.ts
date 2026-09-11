/**
 * 展示层的两个纯格式化函数。
 *
 * 它们放在一起是因为各自的副本数量已经失控：
 *
 * - 时间：`ActivityFeed.tsx` 的 `formatTimestamp` 与 `AgentHistoryPane.tsx` 的
 *   `formatHistoryTime` 逐字节相同，包括那个 `Intl.DateTimeFormat` 选项对象 ——
 *   它是全仓库仅有的两处 `Intl.DateTimeFormat`。
 * - 错误：`useChatSessions.ts` 的 `errorText` 与 `AgentHistoryPane.tsx` 的
 *   `errorMessage` 是同一个表达式，另外还在八个组件里被内联展开成了
 *   `x instanceof Error ? x.message : String(x)`（ActivityFeed 两处、ProjectsPane、
 *   LogsPane、ServicesPane、RemoteFilesPane、ServerOverview、ServerList、TerminalPane）。
 *
 * 十份手抄的「怎么把 unknown 变成一句话」意味着：想统一去掉 zod 的冗长前缀、
 * 或者脱敏 URL 里的凭据，得改十个地方，漏一个就有一处静默地显示不同的文案。
 *
 * 刻意**不**依赖 React 与 Tauri，这样可以直接单测（见 tests/format.test.ts）。
 */

/**
 * `Intl.DateTimeFormat` 的构造不便宜（要装配 locale 数据），而列表里每一行都会
 * 调一次。模块级建一次、反复复用，是这里唯一值得留心的性能点。
 *
 * 只到「月-日 时:分」：这些时间戳出现在一行行记录里，年份和秒对使用者判断
 * 「刚才发生了什么」没有帮助，却会让每一行都变宽。
 */
const TIMESTAMP_FORMATTER = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
});

/**
 * ISO 时间串 → 「08-14 15:32」。
 *
 * 传进来的值来自 Rust 的侧车，正常情况下一定是合法的 ISO 串；但界面上出现过
 * 解析失败的时刻（数据库里是旧格式、时钟回拨等），那时**不能**抛异常 ——
 * 一行时间戳不该让整张列表崩掉。退回原样显示，至少让人看到原始值。
 */
export function formatTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return TIMESTAMP_FORMATTER.format(date);
}

/**
 * 任意抛出物 → 一句可以直接显示给人看的话。
 *
 * 这是 IPC 边界上唯一的兜底：`callDesktop` 会把 Rust 的错误与 zod 的校验失败
 * 都抛出来，它们的类型并不统一（`Error`、字符串、有时是结构化对象）。
 * 不认识的一律 `String()`，不猜结构 —— 猜错会丢掉真正有用的信息。
 */
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
