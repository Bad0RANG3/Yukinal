/** Native task starts own a separate lifecycle; they never claim the foreground chat. */
import { IPC_COMMANDS, type ApprovalDecision, type ApprovalRequest, type IpcCommandMap } from "@yukinal/shared";
import { create } from "zustand";
import { callDesktop, subscribeDesktop, type DesktopEventPayload, type DesktopSubscription } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { RunLifecycle } from "./run-lifecycle.js";

type EventName = "agent.started" | "agent.waiting_approval" | "agent.approval_expired" | "agent.completed" | "agent.failed";
type TaskEvent = DesktopEventPayload<EventName>;
type Detail = IpcCommandMap["investigation_task_get"]["response"];
type StartResponse = IpcCommandMap["investigation_task_start"]["response"];
type StopResponse = IpcCommandMap["investigation_task_stop"]["response"];
const CHANNELS: EventName[] = ["agent.started", "agent.waiting_approval", "agent.approval_expired", "agent.completed", "agent.failed"];
const ACTIVE = new Set(["admitted", "running", "waiting_user"]);
const MAX_RUNS = 64;
const MAX_EARLY_EVENTS = 32;
const MAX_EARLY_BYTES = 256 * 1024;
const MAX_APPROVAL_BYTES = 128 * 1024;

export interface NativeTaskRun {
  runId: string;
  taskId: string;
  sessionId: string;
  serverId: string | null;
  title: string;
  state: "running" | "waiting_approval" | "completed" | "failed" | "cancelled" | "interrupted";
  approval?: ApprovalRequest;
  decision?: string;
  pending: boolean;
  error?: string;
}
export type NativeTaskSnapshot = { runs: NativeTaskRun[]; error: string | null };
export interface NativeTaskRunApi {
  subscribe<E extends EventName>(name: E, handler: (event: DesktopEventPayload<E>) => void): DesktopSubscription;
  start(taskId: string): Promise<StartResponse>;
  detail(taskId: string): Promise<Detail>;
  stop(taskId: string): Promise<StopResponse>;
  respond(params: IpcCommandMap["agent_approval_respond"]["params"]): Promise<{ accepted: boolean }>;
}
const desktopApi: NativeTaskRunApi = {
  subscribe: subscribeDesktop,
  start: (taskId) => callDesktop(IPC_COMMANDS.investigationTaskStart, { taskId }),
  detail: (taskId) => callDesktop(IPC_COMMANDS.investigationTaskGet, { taskId }),
  stop: (taskId) => callDesktop(IPC_COMMANDS.investigationTaskStop, { taskId }),
  respond: (params) => callDesktop(IPC_COMMANDS.agentApprovalRespond, params),
};
type Admission = { events: TaskEvent[]; bytes: number; overflow: boolean };
type TrackedRun = { view: NativeTaskRun; lifecycle: RunLifecycle };

/** Buffers only explicit starts from this window, then verifies the host's actual session binding. */
export class NativeTaskRunRuntime {
  private subscriptions: DesktopSubscription[] = [];
  private ready: Promise<void> | null = null;
  private generation = 0;
  private admissions = new Map<string, Admission>();
  private tracked = new Map<string, TrackedRun>();
  private locks = new Set<string>();
  private error: string | null = null;

  constructor(private publish: (snapshot: NativeTaskSnapshot) => void,
    private api: NativeTaskRunApi = desktopApi, private now: () => number = Date.now) {}

  private emit(): void { this.publish({ runs: [...this.tracked.values()].map((run) => run.view), error: this.error }); }
  connect(): Promise<void> {
    if (this.ready) return this.ready;
    const generation = this.generation;
    this.subscriptions = CHANNELS.map((name) => this.api.subscribe(name, (event) => {
      if (generation === this.generation) this.receive(event);
    }));
    this.ready = Promise.all(this.subscriptions.map((subscription) => subscription.ready)).then(() => undefined).catch((cause: unknown) => {
      if (generation === this.generation) {
        this.subscriptions.forEach((subscription) => subscription.stop());
        this.subscriptions = [];
        this.ready = null;
        this.error = `目标任务事件连接失败：${errorMessage(cause)}`;
        this.emit();
      }
      throw cause;
    });
    return this.ready;
  }
  disconnect(): void {
    this.generation += 1;
    this.subscriptions.forEach((subscription) => subscription.stop());
    this.subscriptions = [];
    this.ready = null;
    this.admissions.clear();
    this.tracked.clear();
    this.locks.clear();
    this.error = null;
    this.emit();
  }
  interrupt(): void {
    this.admissions.clear();
    for (const run of this.tracked.values()) {
      if (!run.lifecycle.running) continue;
      run.lifecycle.fail();
      run.view = { ...run.view, state: "interrupted", approval: undefined, pending: false };
    }
    this.emit();
  }
  private receive(event: TaskEvent): void {
    const run = this.tracked.get(event.runId);
    if (run) { this.apply(run, event); return; }
    const taskId = "taskId" in event ? event.taskId : undefined;
    const admission = taskId ? this.admissions.get(taskId) : [...this.admissions.values()].find((item) => item.events.some((prior) => prior.runId === event.runId));
    if (!admission || admission.overflow) return;
    const bytes = new TextEncoder().encode(JSON.stringify(event)).length;
    if (admission.events.length >= MAX_EARLY_EVENTS || admission.bytes + bytes > MAX_EARLY_BYTES) {
      admission.overflow = true;
      admission.events = [];
      return;
    }
    admission.events.push(event);
    admission.bytes += bytes;
  }
  private apply(run: TrackedRun, event: TaskEvent): void {
    if (("taskId" in event && event.taskId && event.taskId !== run.view.taskId) || !run.lifecycle.isActive(event.runId)) return;
    if (event.type === "agent.waiting_approval") {
      const approval = event.approval;
      if (approval.runId !== run.view.runId || (approval.target.serverId ?? null) !== run.view.serverId) return;
      if (run.view.approval?.approvalId === approval.approvalId && (run.view.pending || run.view.decision)) return;
      if (new TextEncoder().encode(JSON.stringify(approval)).length > MAX_APPROVAL_BYTES) {
        run.view = { ...run.view, approval: undefined, error: "本次审批内容超过界面接纳上限，请停止任务后检查。" };
      } else {
        run.view = { ...run.view, state: "waiting_approval", approval, pending: false, decision: undefined, error: undefined };
      }
    } else if (event.type === "agent.approval_expired") {
      if (run.view.approval?.approvalId !== event.approvalId) return;
      run.view = { ...run.view, decision: "审批已过期", pending: false };
    } else if (event.type === "agent.completed" || event.type === "agent.failed") {
      if (event.type === "agent.completed" && event.result.runId !== event.runId) return;
      if (event.type === "agent.completed" && !["completed", "failed", "cancelled"].includes(event.result.state)) return;
      run.lifecycle.finish(event.runId);
      run.view = { ...run.view, state: event.type === "agent.failed" ? "failed" : event.result.state === "cancelled" ? "cancelled" : event.result.state === "completed" ? "completed" : "failed", pending: false,
        decision: run.view.decision ?? "本次运行已结束" };
    }
    this.emit();
  }
  private trim(): void {
    for (const [id, run] of this.tracked) {
      if (this.tracked.size + this.admissions.size < MAX_RUNS) break;
      if (!run.lifecycle.running) this.tracked.delete(id);
    }
    if (this.tracked.size + this.admissions.size >= MAX_RUNS) throw new Error("本窗口仍有较多进行中的任务，请先停止不再需要的任务。");
  }
  async start(taskId: string): Promise<StartResponse> {
    if (this.admissions.has(taskId) || this.admissions.size >= 16) throw new Error("任务正在受理，请等待本次启动完成。");
    this.trim();
    const admission: Admission = { events: [], bytes: 0, overflow: false };
    this.admissions.set(taskId, admission);
    const generation = this.generation;
    try {
      await this.connect();
      if (generation !== this.generation || this.admissions.get(taskId) !== admission) throw new Error("任务事件连接已关闭，未重复启动。");
      const response = await this.api.start(taskId);
      if (!response.started) throw new Error("宿主未启动本次任务，请检查任务状态。");
      let detail: Detail;
      try { detail = await this.api.detail(taskId); }
      catch (cause) { throw new Error(`任务已启动，但未能读取其真实运行和会话；可使用“停止任务”，不要重复启动：${errorMessage(cause)}`); }
      const saved = detail.runs.find((run) => run.id === response.runId && run.taskId === taskId);
      if (generation !== this.generation || this.admissions.get(taskId) !== admission || admission.overflow ||
          detail.task.id !== taskId || !saved?.sessionId ||
          (ACTIVE.has(saved.status) && detail.task.activeRunId !== response.runId)) {
        throw new Error("任务已启动，但其真实运行和会话未能核验；请停止任务后检查，不要重复启动。");
      }
      const lifecycle = new RunLifecycle();
      lifecycle.begin(response.runId);
      lifecycle.acknowledge(response.runId);
      const active = ACTIVE.has(saved.status) && !["completed", "failed", "stopped", "expired"].includes(detail.task.status);
      if (!active) lifecycle.finish(response.runId);
      const tracked: TrackedRun = { lifecycle, view: { runId: response.runId, taskId, sessionId: saved.sessionId,
        serverId: detail.task.serverId ?? null, title: detail.task.objective.slice(0, 160),
        state: active ? "running" : saved.status === "completed" ? "completed" : saved.status === "cancelled" ? "cancelled" : "failed", pending: false } };
      this.tracked.set(response.runId, tracked);
      this.error = null;
      for (const event of admission.events) if (event.runId === response.runId) this.apply(tracked, event);
      this.emit();
      return response;
    } finally {
      if (this.admissions.get(taskId) === admission) this.admissions.delete(taskId);
    }
  }
  async stop(taskId: string): Promise<StopResponse> {
    const response = await this.api.stop(taskId);
    if (response.task.id === taskId && ["stopped", "completed", "failed", "expired"].includes(response.task.status)) {
      this.admissions.delete(taskId);
      for (const [id, run] of this.tracked) if (run.view.taskId === taskId) this.tracked.delete(id);
      this.emit();
    }
    return response;
  }
  async respond(approval: ApprovalRequest, decision: ApprovalDecision): Promise<void> {
    const run = this.tracked.get(approval.runId);
    if (!run || !run.lifecycle.isActive(approval.runId) || run.view.state !== "waiting_approval" ||
        run.view.approval !== approval || run.view.decision || this.locks.has(approval.approvalId) || (decision === "approve_session" && approval.sessionGrantable === false)) return;
    if (Date.parse(approval.expiresAt) <= this.now()) {
      run.view = { ...run.view, decision: "审批已过期" }; this.emit(); return;
    }
    this.locks.add(approval.approvalId);
    run.view = { ...run.view, pending: true, error: undefined }; this.emit();
    try {
      const response = await this.api.respond({ runId: approval.runId, approvalId: approval.approvalId, decision, respondedAt: new Date(this.now()).toISOString() });
      if (this.tracked.get(approval.runId) === run && run.view.approval === approval) {
        run.view = { ...run.view, pending: false, decision: !response.accepted ? "审批已失效" : decision === "reject" ? "已拒绝" : decision === "approve_session" ? "已批准本次运行" : "已批准一次" };
      }
    } catch (cause) {
      if (this.tracked.get(approval.runId) === run && run.view.approval === approval) run.view = { ...run.view, pending: false, error: `审批提交失败：${errorMessage(cause)}` };
    } finally {
      this.locks.delete(approval.approvalId);
      this.emit();
    }
  }
}

export const useNativeTaskRunStore = create<NativeTaskSnapshot>(() => ({ runs: [], error: null }));
export const nativeTaskRuns = new NativeTaskRunRuntime((snapshot) => useNativeTaskRunStore.setState(snapshot));

/** The task page uses task identity; chat additionally requires its actual host-read session and server. */
export function visibleNativeTaskRuns(runs: readonly NativeTaskRun[], scope: { taskId?: string; sessionId?: string | null; serverId?: string | null }): NativeTaskRun[] {
  return runs.filter((run) => scope.taskId ? run.taskId === scope.taskId : Boolean(scope.sessionId) && run.sessionId === scope.sessionId && run.serverId === (scope.serverId ?? null));
}
