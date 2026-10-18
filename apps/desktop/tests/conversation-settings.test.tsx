import assert from "node:assert/strict";
import test from "node:test";
import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ConversationSettings, type ConversationSettingsProps } from "../src/features/agent/ConversationSettings.js";
import { defaultAgentRoles } from "../src/features/agent-nist/agent-nist.js";

const base: ConversationSettingsProps = {
  id: "conversation-settings", open: true, closing: false, running: false, panelRef: { current: null },
  onAnimationEnd: () => {}, onClose: () => {}, runMode: "goal", onRunModeChange: () => {},
  permissionMode: "ask", onPermissionModeChange: () => {}, runPolicy: null, onRunPolicyChange: () => {},
  targetEnvironment: "unknown", delivery: "async", onDeliveryChange: () => {},
  roles: defaultAgentRoles(), selectedRoleId: null, onSelectRole: () => {},
};
type Element = ReactElement<Record<string, any>>;
function find(node: ReactNode, match: (element: Element) => boolean): Element {
  const visit = (value: ReactNode): Element | undefined => {
    if (Array.isArray(value)) { for (const item of value) { const hit = visit(item); if (hit) return hit; } return; }
    if (!isValidElement<Record<string, any>>(value)) return;
    return match(value) ? value : visit(value.props.children);
  };
  const result = visit(node);
  assert.ok(result, "requested control must be reachable in the actual component tree");
  return result;
}
const named = (tree: ReactNode, label: string) => find(tree, (element) => element.props["aria-label"] === label);

test("settings expose all real run modes and keep policy mismatch visible before advanced disclosure", () => {
  const markup = renderToStaticMarkup(<ConversationSettings {...base} runMode="plan" runPolicy="policy.development" targetEnvironment="production" />);
  assert.match(markup, /<option value="goal">目标模式<\/option>/);
  assert.match(markup, /<option value="plan" selected="">计划模式<\/option>/);
  assert.match(markup, /<option value="readonly">只读模式<\/option>/);
  assert.ok(markup.indexOf('role="status"') < markup.indexOf('<details'), "policy mismatch must not be hidden in advanced settings");
  assert.match(markup, /目标环境是生产，但选择了开发策略/);
  assert.doesNotMatch(markup, /<details[^>]+open=/);
  assert.doesNotMatch(markup, /<optgroup/);
});

test("closed and running settings reject late events without changing the next request", () => {
  for (const state of [{ open: false, closing: true, running: false }, { open: true, closing: false, running: true }]) {
    const calls: unknown[] = [];
    const tree = ConversationSettings({ ...base, ...state, onRunModeChange: (v) => calls.push(v), onPermissionModeChange: (v) => calls.push(v), onRunPolicyChange: (v) => calls.push(v), onSelectRole: (v) => calls.push(v), onDeliveryChange: (v) => calls.push(v) });
    for (const [label, value] of [["运行方式", "goal"], ["批准方式", "auto"], ["目标策略", "policy.development"], ["处理角色", base.roles[0]!.id]]) {
      const control = named(tree, label!);
      assert.equal(control.props.disabled, true);
      control.props.onChange({ target: { value } });
    }
    find(tree, (e) => e.type === "button" && e.props.children === "等待完成").props.onClick();
    assert.deepEqual(calls, []);
  }
  const closed = renderToStaticMarkup(<ConversationSettings {...base} open={false} closing />);
  assert.match(closed, /aria-hidden="true" inert=""/);
});

test("editable settings preserve mode, approval, role and environment-auto payload semantics", () => {
  const calls: unknown[] = [];
  const roleId = base.roles[0]!.id;
  const tree = ConversationSettings({ ...base, onRunModeChange: (v) => calls.push(["mode", v]), onPermissionModeChange: (v) => calls.push(["approval", v]), onRunPolicyChange: (v) => calls.push(["policy", v]), onSelectRole: (v) => calls.push(["role", v]), onDeliveryChange: (v) => calls.push(["delivery", v]) });
  for (const [label, value] of [["运行方式", "plan"], ["批准方式", "auto"], ["目标策略", "policy.production"], ["目标策略", "environment"], ["处理角色", roleId], ["处理角色", ""]]) named(tree, label!).props.onChange({ target: { value } });
  find(tree, (e) => e.type === "button" && e.props.children === "等待完成").props.onClick();
  assert.deepEqual(calls, [["mode", "plan"], ["approval", "auto"], ["policy", "policy.production"], ["policy", null], ["role", roleId], ["role", null], ["delivery", "sync"]]);
});

test("running settings still allow an explicit close while a closing panel rejects repeated close", () => {
  let closed = 0;
  const callback = () => closed++;
  named(ConversationSettings({ ...base, running: true, onClose: callback }), "关闭对话设置").props.onClick();
  named(ConversationSettings({ ...base, open: false, closing: true, onClose: callback }), "关闭对话设置").props.onClick();
  assert.equal(closed, 1);
});
