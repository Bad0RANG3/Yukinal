import { useId } from "react";
import { HEALTH_THRESHOLDS, healthClass, type ServerSnapshot } from "@yukinal/shared";

import { resourceTrendSegments, resourceValues, type ResourceMetric, type ResourceSample } from "./resource-trends.js";

const METRICS = [
  { key: "cpu", label: "CPU" },
  { key: "memory", label: "内存" },
  { key: "disk", label: "磁盘", note: "最高占用" },
] as const;

const percent = (value: number) => `${new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 1 }).format(value)}%`;
const clock = (value: string) => new Intl.DateTimeFormat("zh-CN", {
  hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
}).format(new Date(value));

function ResourceChart({ metric, label, note, current, samples }: {
  metric: ResourceMetric;
  label: string;
  note?: string;
  current?: number;
  samples: readonly ResourceSample[];
}) {
  const titleId = useId();
  const segments = resourceTrendSegments(samples, metric);
  const points = segments.flat();
  const state = current === undefined ? "unknown" : healthClass(current, HEALTH_THRESHOLDS[metric]);
  const valueLabel = current === undefined ? "读数不可用" : percent(current);
  const description = points.length
    ? `${points.length} 次有效采样，纵轴 0 到 100%。${points.length === 1 ? "只有一个采样点，等待积累趋势。" : "缺失读数或采集中断处不连线。"}`
    : "尚无可用采样，不绘制趋势。";
  return <figure className={`overview-resource-trend overview-resource-trend-${state}`}>
    <figcaption><span>{label}{note ? <small>{note}</small> : null}</span><strong aria-label={`当前${label}${valueLabel}`}>{current === undefined ? "—" : percent(current)}</strong></figcaption>
    <div className="overview-resource-plot">
      <svg viewBox="0 0 240 54" preserveAspectRatio="none" role="img" aria-labelledby={titleId}>
        <title id={titleId}>{`${label}使用率趋势，当前${valueLabel}。${description}`}</title>
        <path className="overview-trend-grid" d="M4 4H236 M4 27H236 M4 50H236" />
        {segments.map((segment, index) => {
          const last = segment.at(-1)!;
          return <g key={index}>
            {segment.length > 1 ? <path className="overview-trend-line" d={segment.map((point, pointIndex) => `${pointIndex ? "L" : "M"}${point.x.toFixed(2)} ${point.y.toFixed(2)}`).join(" ")} /> : null}
            <circle className="overview-trend-point" cx={last.x} cy={last.y} r="2.5"><title>{`${clock(last.collectedAt)} · ${percent(last.value)}`}</title></circle>
          </g>;
        })}
      </svg>
      {!points.length ? <span className="overview-trend-empty">读数不可用</span> : null}
    </div>
  </figure>;
}

export function ResourceTrends({ snapshot, samples }: { snapshot: ServerSnapshot; samples: readonly ResourceSample[] }) {
  const current = resourceValues(snapshot);
  const first = samples[0];
  const last = samples.at(-1);
  return <section className="overview-resources" aria-label="CPU、内存与磁盘使用率趋势">
    <div className="overview-resource-heading"><h3>资源趋势</h3><span>{samples.length} 次采样</span></div>
    <div className="overview-resource-list">{METRICS.map((metric) => <ResourceChart key={metric.key} metric={metric.key} label={metric.label} note={"note" in metric ? metric.note : undefined} current={current[metric.key]} samples={samples} />)}</div>
    <div className="overview-trend-period">{first && last && samples.length > 1
      ? <><span><time dateTime={first.collectedAt}>{clock(first.collectedAt)}</time> — <time dateTime={last.collectedAt}>{clock(last.collectedAt)}</time></span><span>本窗口实测</span></>
      : <span>{samples.length ? "首个采样，等待下一次更新" : "等待有效采样时间"}</span>}</div>
  </section>;
}
