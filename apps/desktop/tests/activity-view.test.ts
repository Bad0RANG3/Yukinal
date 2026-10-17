import assert from "node:assert/strict";
import { test } from "node:test";
import type { Activity, ToolExecutionRecord } from "@yukinal/shared";
import { activityClock, activityExecutionSummary, groupActivitiesByDay, isActivityIssue } from "../src/features/activity/activity-view.js";

function activity(id: string, date: Date | string, outcome?: Activity["outcome"]): Activity {
  return { id, type: "connection", title: id, source: "user", actor: "user", createdAt: typeof date === "string" ? date : date.toISOString(), outcome };
}

test("activity groups use local calendar dates, newest first, without mutating cached rows", () => {
  const rows = [activity("yesterday", new Date(2026, 9, 16, 23, 59)), activity("early", new Date(2026, 9, 17, 0, 1)), activity("older", new Date(2025, 9, 17, 14)), activity("latest", new Date(2026, 9, 17, 14))];
  const groups = groupActivitiesByDay(rows, new Date(2026, 9, 17, 15));
  assert.deepEqual(groups.map((group) => group.label), ["今天", "昨天", "2025年10月17日"]);
  assert.deepEqual(groups[0]?.activities.map((row) => row.id), ["latest", "early"]);
  assert.deepEqual(rows.map((row) => row.id), ["yesterday", "early", "older", "latest"]);
  assert.equal(activityClock(rows[1]!.createdAt), "00:01");
});

test("year boundaries and invalid timestamps keep events visible in the correct groups", () => {
  const groups = groupActivitiesByDay([activity("unknown", "old-format"), activity("yesterday", new Date(2025, 11, 31, 23, 59)), activity("earlier", new Date(2025, 11, 30))], new Date(2026, 0, 1, 0, 1));
  assert.deepEqual(groups.map((group) => group.label), ["昨天", "2025年12月30日", "时间未知"]);
  assert.equal(groups.at(-1)?.activities[0]?.id, "unknown");
  assert.equal(activityClock("old-format"), "—");
});

test("issue filter includes failed and denied events without relabeling cancellations", () => {
  const rows = [activity("failed", "2026-10-17", "failure"), activity("denied", "2026-10-17", "denied"), activity("cancelled", "2026-10-17", "cancelled"), activity("success", "2026-10-17", "success"), activity("unknown", "2026-10-17")];
  assert.deepEqual(rows.filter(isActivityIssue).map((row) => row.id), ["failed", "denied"]);
});

test("execution previews retain uncertainty and errors without dumping structured payloads", () => {
  const execution = (output: unknown, error?: string) => ({ output, error }) as ToolExecutionRecord;
  assert.equal(activityExecutionSummary(execution({ executionState: "timed_out", summary: "重启请求已发出" })), "执行超时，效果可能已发生 · 重启请求已发出");
  assert.equal(activityExecutionSummary(execution({ executionState: "result_unknown", summary: "连接已中断" })), "执行结果未知 · 连接已中断");
  assert.equal(activityExecutionSummary(execution({ executionState: "cancelled", summary: "等待确认" }, "调用已取消")), "执行已取消，效果可能已发生 · 调用已取消");
  assert.equal(activityExecutionSummary(execution({ summary: "读取完成" }, "权限不足")), "权限不足");
  assert.equal(activityExecutionSummary(execution({ rows: [1, 2, 3] })), "查看输入与完整结果");
  assert.equal(activityExecutionSummary(execution("读取到 3 条日志")), "读取到 3 条日志");
});
