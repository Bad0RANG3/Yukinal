import { useState, type ReactNode } from "react";
import type { ConversationWidget, ConversationWidgetDocument } from "../lib/conversation-widgets.js";

import { Icon, type IconName } from "./Icon.js";

type StepsWidget = Extract<ConversationWidget, { type: "steps" }>;
type MetricsWidget = Extract<ConversationWidget, { type: "metrics" }>;
type ProgressWidget = Extract<ConversationWidget, { type: "progress" }>;

const STEP_LABELS = { pending: "待处理", active: "进行中", done: "已完成", error: "异常" } as const;
const STEP_ICONS: Record<StepsWidget["items"][number]["status"], IconName> = {
  pending: "activity", active: "activity", done: "check", error: "warning",
};

/** Display only normalized reply data. These are never host approvals or evidence. */
export function ConversationWidgets({ document, streaming = false }: {
  document: ConversationWidgetDocument;
  streaming?: boolean;
}) {
  if (document.widgets.length === 0) return null;
  return <SummaryFrame streaming={streaming}>
    <div className="conversation-widgets-content">
      {document.widgets.map((widget, index) => <WidgetView key={`${index}:${widget.type}`} widget={widget} />)}
    </div>
  </SummaryFrame>;
}

function SummaryFrame({ streaming, children }: { streaming: boolean; children: ReactNode }) {
  const [entering, setEntering] = useState(streaming);
  return <section className={`conversation-widgets${entering ? " is-entering" : ""}`} aria-label="AI 摘要" aria-busy={streaming || undefined}
    onAnimationEnd={(event) => { if (event.target === event.currentTarget) setEntering(false); }}>
    <div className="conversation-widgets-caption">
      <span title="由本条 AI 回复提供的展示数据"><Icon name="sparkle" size="xs" />AI 摘要</span>
      {streaming ? <span className="conversation-widgets-streaming">生成中</span> : null}
    </div>
    {children}
  </section>;
}

function WidgetView({ widget }: { widget: ConversationWidget }) {
  switch (widget.type) {
    case "card":
      return <section className="conversation-widget conversation-widget-card" data-tone={widget.tone}>
        <h4>{widget.title}</h4>
        <p className="conversation-widget-body">{widget.body}</p>
      </section>;
    case "steps": return <Steps widget={widget} />;
    case "metrics": return <Metrics widget={widget} />;
    case "progress": return <Progress widget={widget} />;
    case "details":
      return <details className="conversation-widget conversation-widget-details">
        <summary><Icon name="chevronRight" size="sm" /><span>{widget.title}</span></summary>
        <div className="conversation-widget-disclosure" tabIndex={0} role="region" aria-label={widget.title}><p className="conversation-widget-body">{widget.body}</p></div>
      </details>;
  }
}

function Steps({ widget }: { widget: StepsWidget }) {
  const counts = { pending: 0, active: 0, done: 0, error: 0 };
  for (const item of widget.items) counts[item.status]++;
  return <section className="conversation-widget conversation-widget-steps">
    <h4>{widget.title}</h4>
    <p className="conversation-widget-note">{`共 ${widget.items.length} 项 · 已完成 ${counts.done} · 进行中 ${counts.active} · 待处理 ${counts.pending} · 异常 ${counts.error}`}</p>
    <ol className="conversation-widget-step-list">
      {widget.items.map((item, index) => <li key={index} data-step-status={item.status}>
        <span className="conversation-widget-step-mark"><Icon name={STEP_ICONS[item.status]} size="xs" /></span>
        <div className="conversation-widget-step-copy">
          <div className="conversation-widget-step-heading"><strong>{item.title}</strong><span>{STEP_LABELS[item.status]}</span></div>
          {item.detail ? <p className="conversation-widget-body">{item.detail}</p> : null}
        </div>
      </li>)}
    </ol>
  </section>;
}

function Metrics({ widget }: { widget: MetricsWidget }) {
  return <section className="conversation-widget conversation-widget-metrics">
    <h4>{widget.title}</h4>
    <dl className="conversation-widget-metric-list">
      {widget.items.map((item, index) => <div key={index}>
        <dt>{item.label}</dt>
        <dd><span className="conversation-widget-metric-value">{String(item.value)}</span>{item.unit ? <small>{item.unit}</small> : null}</dd>
        {item.trend && item.trend.length >= 2 && item.trend.every(Number.isFinite)
          ? <dd className="conversation-widget-metric-trend"><Sparkline label={item.label} points={item.trend} /></dd> : null}
      </div>)}
    </dl>
  </section>;
}

/** A line connects only supplied samples; it has no implied clock or scale. */
function Sparkline({ label, points }: { label: string; points: number[] }) {
  let low = points[0]!;
  let high = low;
  for (const value of points) { low = Math.min(low, value); high = Math.max(high, value); }
  // Scale before subtracting to keep even very large finite input ranges finite.
  const scale = Math.max(Math.abs(low), Math.abs(high)) || 1;
  const min = low / scale;
  const range = high / scale - min;
  const coordinates = points.map((value, index) => {
    const x = 2 + index * 116 / (points.length - 1);
    const y = range === 0 ? 16 : 2 + (1 - (value / scale - min) / range) * 28;
    return `${Number(x.toFixed(2))},${Number(y.toFixed(2))}`;
  }).join(" ");
  return <svg className="conversation-widget-sparkline" viewBox="0 0 120 32" preserveAspectRatio="none" role="img" aria-label={`${label}，AI 提供的 ${points.length} 个数据点`}>
    <title>{`数据点：${points.join("、")}`}</title>
    <polyline points={coordinates} vectorEffect="non-scaling-stroke" />
  </svg>;
}

function Progress({ widget }: { widget: ProgressWidget }) {
  const validRange = Number.isFinite(widget.value) && Number.isFinite(widget.max)
    && widget.max > 0 && widget.value >= 0 && widget.value <= widget.max;
  const valueText = `${widget.value} / ${widget.max}`;
  return <section className="conversation-widget conversation-widget-progress">
    <div className="conversation-widget-progress-heading"><h4>{widget.title}</h4><span>{valueText}</span></div>
    {validRange ? <progress value={widget.value} max={widget.max} aria-label={widget.title} aria-valuetext={valueText} /> : null}
    {widget.detail ? <p className="conversation-widget-body">{widget.detail}</p> : null}
  </section>;
}
