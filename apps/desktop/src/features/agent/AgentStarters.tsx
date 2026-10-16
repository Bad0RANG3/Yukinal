import { Icon, type IconName } from "../../components/Icon.js";

const STARTERS: ReadonlyArray<{ icon: IconName; title: string; prompt: string }> = [
  { icon: "activity", title: "检查资源", prompt: "请对当前环境做一次只读资源检查，分析 CPU、内存和磁盘使用情况，说明异常依据与建议。不要修改配置或重启服务。" },
  { icon: "logs", title: "分析日志", prompt: "请帮我分析日志中的异常，先确认需要的日志和时间范围，再整理可能原因与排查步骤。先不要修改文件或重启服务。" },
  { icon: "projects", title: "规划部署", prompt: "请帮我规划一次部署，先询问项目、目标环境和约束，再整理部署步骤、风险与回退方案。只制定方案，暂不执行。" },
];

export function AgentStarters({ blockedReason, onPick }: { blockedReason: string | null; onPick: (prompt: string) => void }) {
  return <div className="agent-starters" aria-label="任务起点">
    <div className="agent-starter-grid">
      {STARTERS.map((starter) => <button
        type="button"
        className="agent-starter"
        key={starter.title}
        disabled={Boolean(blockedReason)}
        title={blockedReason ?? `填入草稿：${starter.title}`}
        onClick={() => onPick(starter.prompt)}
      >
        <span className="agent-starter-icon"><Icon name={starter.icon} size="sm" /></span>
        <span><strong>{starter.title}</strong></span>
      </button>)}
    </div>
  </div>;
}
