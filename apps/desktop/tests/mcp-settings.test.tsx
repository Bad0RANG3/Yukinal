/**
 * MCP 设置面板（ADR 0014）：把「崩掉的服务器说了什么」「http 行说了什么」钉成断言。
 *
 * 这个功能里最要紧的几件事都是**文案与结构**，不是数据流：
 *
 * - 崩过的服务器必须显示退出原因，并且**没有**任何自动重启的按钮（「启动」是用户的动作）；
 * - `http` 行必须显示后端给出的完整拒绝理由，而且没有「忽略并继续」；
 * - 表单里**不提供** http 选项（摆一个保存时才失败的选项等于骗人）；
 * - 「启动」在一个起不来的行上必须是禁用的，而不是点了没反应；
 * - 保存的说明里必须写清「保存不等于信任」。
 *
 * 这些用 Rust 或 zod 都测不到（它们在渲染里），所以这里直接渲染静态 HTML 断言，
 * 与 `host-key.test.tsx` 一致：没有 DOM，所以不需要 jsdom。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import type { McpServerView } from "@yukinal/shared";

import { McpSettingsPanel } from "../src/features/settings/McpSettings.js";
import {
  draftFromServer,
  emptyDraft,
  mcpExitText,
  mcpUnavailableText,
  previewToolNamespace,
  serverMessages,
  statusLabel,
  toSaveInput,
  type McpServerDraft,
} from "../src/lib/mcp.js";

const noop = () => {};

function serverView(overrides: {
  config?: Partial<McpServerView["config"]>;
  status?: Partial<McpServerView["status"]>;
  tools?: McpServerView["tools"];
  unavailable?: McpServerView["unavailable"];
}): McpServerView {
  const config = {
    id: "mcp_1",
    label: "fixture",
    transport: "stdio" as const,
    command: "node",
    args: ["./mcp-server.js", "ok"],
    enabled: true,
    allowedTools: [],
    trustLevel: "unreviewed" as const,
    ...overrides.config,
  };
  return {
    config,
    status: {
      serverId: config.id,
      running: false,
      pid: null,
      program: config.command ?? null,
      startedAt: null,
      protocolVersion: null,
      serverName: null,
      serverVersion: null,
      toolCount: 0,
      lastExit: null,
      stderrTail: [],
      diagnostics: [],
      ...overrides.status,
    },
    tools: overrides.tools ?? [],
    ...(overrides.unavailable === undefined ? {} : { unavailable: overrides.unavailable }),
  };
}

function render(overrides: Partial<Parameters<typeof McpSettingsPanel>[0]> = {}): string {
  return renderToStaticMarkup(
    <McpSettingsPanel
      servers={[]}
      draft={emptyDraft()}
      onDraftChange={noop}
      onBeginNew={noop}
      onBeginEdit={noop}
      onCancelEdit={noop}
      onSubmit={noop}
      onStart={noop}
      onStop={noop}
      onDelete={noop}
      {...overrides}
    />,
  );
}

interface RenderedButton {
  text: string;
  disabled: boolean;
}

/** 按按钮切分再断言，避免「页面上别处也有 disabled」这种假通过。 */
function buttons(html: string): RenderedButton[] {
  return [...html.matchAll(/<button([^>]*)>([\s\S]*?)<\/button>/g)].map((match) => ({
    text: (match[2] ?? "").replace(/<[^>]*>/g, "").replace(/\s+/g, ""),
    disabled: /\bdisabled\b/.test(match[1] ?? ""),
  }));
}

function buttonNamed(html: string, label: string): RenderedButton {
  const found = buttons(html).find((button) => button.text.includes(label));
  assert.ok(found, `渲染结果里没有「${label}」按钮：${buttons(html).map((b) => b.text).join(" / ")}`);
  return found;
}

/* ── 纯规则 ───────────────────────────────────────────────────────────────── */

test("a crashed server is described by its exit record, not as merely 'not running'", () => {
  const crashed = serverView({
    status: {
      lastExit: { code: 7, signal: null, at: "2025-01-01T00:00:00Z", reason: "exit code 7" },
      stderrTail: ["boom"],
    },
  });

  assert.deepEqual(statusLabel(crashed), { running: false, text: "已退出" });
  assert.deepEqual(statusLabel(serverView({})), { running: false, text: "未启动" });
  assert.match(mcpExitText(crashed) ?? "", /exit code 7/);
  assert.match(mcpExitText(crashed) ?? "", /2025-01-01T00:00:00Z/);
  assert.match(serverMessages(crashed).join("\n"), /stderr: boom/);
  // 没崩过就什么都不说：一句「上次退出：null」比沉默更糟。
  assert.equal(mcpExitText(serverView({})), null);
});

test("the transport preview is a hint, not a second validator", () => {
  assert.equal(previewToolNamespace("mcp_1"), "mcp.mcp-1.*");
  assert.equal(previewToolNamespace("Mcp-One"), "mcp.mcp-one.*");
  // 后端才会拒绝的输入：这里不假装自己知道结果，返回 null 让界面只说「必须能变成一段」。
  assert.equal(previewToolNamespace("mcp..1"), null);
  assert.equal(previewToolNamespace(""), null);

  // 拒绝理由原样来自后端：界面不重写它（重写会造出第二个真相来源）。
  const http = serverView({
    unavailable: { code: "transport_not_implemented", message: "outbound network policy is not defined yet" },
  });
  assert.equal(mcpUnavailableText(http), "outbound network policy is not defined yet");
  assert.equal(mcpUnavailableText(serverView({})), null);
});

test("the draft round-trips a stored row and never rewrites the parts it does not own", () => {
  const view = serverView({ config: { args: ["a", "--label", "b,c"] } });
  const draft = draftFromServer(view.config);
  assert.equal(draft.args, "a\n--label\nb,c");
  assert.deepEqual(toSaveInput(draft).args, ["a", "--label", "b,c"], "逗号是参数的一部分，不是分隔符");

  // 空行不是参数，前后空白也不是。
  const messy: McpServerDraft = { ...draft, command: "  node  ", args: "  a \n\n  b  \n" };
  const input = toSaveInput(messy);
  assert.equal(input.command, "node");
  assert.deepEqual(input.args, ["a", "b"]);
  // 表单不发送 allowedTools / trustLevel：它们由后端保留原值。
  assert.deepEqual(Object.keys(input).sort(), ["args", "command", "enabled", "id", "label", "transport"]);
});

/* ── 渲染 ─────────────────────────────────────────────────────────────────── */

test("a running server shows its pid, protocol version and declared tools", () => {
  const html = render({
    servers: [
      serverView({
        status: { running: true, pid: 4242, protocolVersion: "2025-11-25", toolCount: 2 },
        // 这是**服务器自己的拼写**（`tools/list` 原样回来的），不是内部名：宿主那边的
        // `McpToolDescriptor.name` 就是 `"echo"`，内部名 `mcp.mcp-1.echo` 是注册时派生的。
        // 界面照着说，不能说成「已注册」或把内部名写出来。
        tools: [
          { name: "echo", description: "echo", inputSchema: {} },
          { name: "explode", description: "boom", inputSchema: {} },
        ],
      }),
    ],
  });

  assert.match(html, /running|运行中/);
  assert.match(html, /4242/);
  assert.match(html, /2025-11-25/);
  assert.match(html, /服务器声明了 2 个工具/);
  assert.match(html, /echo/);
  assert.match(html, /explode/);
  assert.ok(!/已注册工具/.test(html), "描述符不代表 agent 已经注册了它们");
  assert.ok(!buttons(html).some((button) => button.text.includes("启动")), "跑着的服务器不该有「启动」按钮");
  assert.ok(buttonNamed(html, "停止"));
});

test("a crashed server shows why, offers no auto-restart, and keeps the exit record visible", () => {
  const html = render({
    servers: [
      serverView({
        status: {
          lastExit: { code: 7, signal: null, at: "2025-01-01T00:00:00Z", reason: "exit code 7" },
          stderrTail: ["failed to bind"],
        },
        unavailable: {
          code: "exited",
          message:
            'mcp server "mcp_1" exited (exit code 7) at 2025-01-01T00:00:00Z and is deliberately not restarted: a restart can re-execute side effects. Start it explicitly if you want it back.',
        },
      }),
    ],
  });

  assert.match(html, /exit code 7/);
  assert.match(html, /deliberately not restarted/);
  assert.match(html, /failed to bind/);
  // 唯一的出路是用户按「启动」，而那不是自动恢复。
  assert.ok(buttonNamed(html, "启动"));
  assert.ok(!/自动重启|重试|retry/i.test(html), "界面上不能出现任何自动恢复的暗示");
});

test("an http row is shown with the core's refusal and cannot be started", () => {
  const html = render({
    servers: [
      serverView({
        config: { id: "mcp_http", transport: "http", command: undefined, args: undefined, url: "https://mcp.example.com" },
        unavailable: {
          code: "transport_not_implemented",
          message:
            'mcp server "mcp_http" uses transport "http", which this build does not implement: outbound network policy is not defined yet.',
        },
      }),
    ],
  });

  assert.match(html, /outbound network policy/);
  assert.match(html, /http/);
  assert.ok(buttonNamed(html, "启动").disabled, "一个起不来的行不该摆出能点的「启动」");
  assert.ok(!/忽略|仍然继续|ignore/i.test(html), "没有「忽略并继续」这条路");
});

test("a disabled row cannot be started either, and says so in the title", () => {
  const html = render({ servers: [serverView({ config: { enabled: false } })] });
  const start = buttonNamed(html, "启动");
  assert.ok(start.disabled);
  assert.match(html, /被禁用/);
});

test("the form offers stdio only, and says why http is absent", () => {
  const html = render({ editingId: "mcp_1", draft: draftFromServer(serverView({}).config) });

  assert.match(html, /一行一个/);
  assert.match(html, /保存\*\*不会启动\*\*服务器|保存/);
  assert.match(html, /每个 MCP 工具调用都需要用户逐项批准/);
  assert.match(html, /不提供 http 传输/);
  // 表单里没有任何 http 单选/下拉选项。
  assert.ok(!/<option[^>]*value="http"/.test(html));
  assert.ok(!/name="transport"/.test(html));
});

test("an empty list says what to do instead of showing an empty table", () => {
  const html = render({ servers: [] });
  assert.match(html, /还没有配置任何 MCP 服务器/);
  assert.match(html, /npx/);
});

test("errors and notices are rendered with the roles a screen reader needs", () => {
  const html = render({ error: "请在 Yukinal 桌面应用中执行此操作（mcp_server_list）。", notice: "已保存。" });
  assert.match(html, /role="alert"/);
  assert.match(html, /role="status"/);
  assert.match(html, /mcp_server_list/);
});
