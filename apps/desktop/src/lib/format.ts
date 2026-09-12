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
 * ISO 时间串 → 「刚刚」「12 分钟前」「3 小时前」「2 天前」，更久则回落到
 * `formatTimestamp` 的「08-14 15:32」。
 *
 * 两条刻意的边界：
 *
 * - **只相对到「天」，不再说「3 周前」。** 超过一周之后，相对说法反而更难用：
 *   人判断「这件事多久以前发生的」时，超过一周就开始换算成日期，而「23 天前」
 *   还要求对方再减一次。
 * - **未来时间算「刚刚」。** 记录里的时间是远端宿主写的，本机时钟回拨或两侧时钟
 *   不一致时会出现「刚发生的事来自 3 分钟后」。显示「-2 分钟前」是谎话，直接报错
 *   更糟 —— 一条时间戳不该让整张列表崩掉，所以按「刚刚」处理。
 *
 * `now` 可注入，因此这个函数可以单测，不必依赖当前时间。
 */
export function formatRelativeTime(value: string, now: number = Date.now()): string {
  const time = new Date(value).getTime();
  if (Number.isNaN(time)) return value;
  const elapsed = now - time;
  if (elapsed < MINUTE_MS) return "刚刚";
  if (elapsed < HOUR_MS) return `${Math.floor(elapsed / MINUTE_MS)} 分钟前`;
  if (elapsed < DAY_MS) return `${Math.floor(elapsed / HOUR_MS)} 小时前`;
  if (elapsed < WEEK_MS) return `${Math.floor(elapsed / DAY_MS)} 天前`;
  return formatTimestamp(value);
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const WEEK_MS = 7 * DAY_MS;

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

/**
 * 字节数 → 「128 B」「1.5 KB」「5.6 GB」。
 *
 * 合并自两份实现，它们的**适用范围**不同，所以合并本身修掉了一个真缺陷：
 *
 * - `RemoteFilesPane` 的 `formatSize` 只到 MB 就停了。远程目录里一个 5 GB 的
 *   镜像文件会显示成「5120.0 MB」—— 数值没说错，但没人能一眼读出量级，而这正是
 *   文件列表要回答的问题。
 * - `ServerOverview` 的 `formatBytes` 一路走到 TB，但在 KB 上舍入方式不同：
 *   它保留一位小数，`formatSize` 直接四舍五入。同一个 1536 字节的文件，
 *   一边说「1.5 KB」，另一边说「2 KB」。
 *
 * 采用后者的规则，因为「10 以下保留一位小数、10 以上取整」是列表里更好扫读的
 * 做法（宽度稳定），而 MB 封顶是没有理由的。
 *
 * **这会改变文件列表的显示**：1536 字节从「2 KB」变成「1.5 KB」——
 * 信息不再在显示层被抹掉；同时 GB/TB 现在有名字了。这是有意的，不是副作用。
 *
 * `undefined` 与无法表示的数都退回「—」：概览页里的内存/磁盘读数来自采集器，
 * 采集器没返回数据是常态，那时显示「NaN B」比显示破折号糟得多。
 */
export function formatBytes(bytes?: number): string {
  if (bytes === undefined || !Number.isFinite(bytes)) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // 取整阈值放在 10：两位数以上再带小数会让每一行的宽度忽长忽短。
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}
