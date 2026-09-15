/**
 * MCP 设置面板（ADR 0014）：把「崩掉的服务器说了什么」「HTTP 端点说了什么」钉成断言。
 *
 * 这个功能里最要紧的几件事都是**文案与结构**，不是数据流：
 *
 * - 崩过的服务器必须显示退出原因，并且**没有**任何自动重启的按钮（「启动」是用户的动作）；
 * - 无效 HTTP 端点必须显示后端给出的完整理由，而且没有「忽略并继续」；
 * - 表单在 stdio 与 Streamable HTTP 之间切换对应字段；
 * - 「启动」在一个起不来的行上必须是禁用的，而不是点了没反应；
 * - 保存的说明里必须写清「保存不等于信任」。
 *
 * 这些用 Rust 或 zod 都测不到（它们在渲染里），所以这里直接渲染静态 HTML 断言，
 * 与 `host-key.test.tsx` 一致：没有 DOM，所以不需要 jsdom。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import type { McpOAuthConfig, McpOAuthDeviceCodeEvent, McpServerView } from "@yukinal/shared";

import { McpSettingsPanel } from "../src/features/settings/McpSettings.js";
import {
  McpOAuthDeviceCodeView,
  type McpOAuthDeviceCodePhase,
} from "../src/features/settings/McpOAuthDeviceCodeDialog.js";
import {
  deviceCodeExpiryText,
  deviceCodeLink,
  draftFromServer,
  emptyDraft,
  mcpExitText,
  mcpRestartText,
  mcpUnavailableText,
  oauthClientAuthNeedsSecret,
  previewToolNamespace,
  serverMessages,
  statusLabel,
  toSaveInput,
  type McpServerDraft,
} from "../src/lib/mcp.js";

const noop = () => {};

/**
 * 一份已存的 OAuth 配置：公共客户端 + 授权码流程。测试只覆盖自己关心的那几项，
 * 于是新增一个非机密字段时不必改六处字面量。
 */
function oauthConfig(overrides: Partial<McpOAuthConfig> = {}): McpOAuthConfig {
  return {
    issuer: "https://auth.example.com",
    clientId: "desktop-client",
    flow: "authorization_code",
    clientAuth: "none",
    dpop: false,
    scopes: ["mcp.read"],
    ...overrides,
  };
}

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
    httpAuthHeaders: [],
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
      onReview={noop}
      onOAuthConnect={noop}
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
    unavailable: { code: "invalid_config", message: "remote endpoints must use HTTPS" },
  });
  assert.equal(mcpUnavailableText(http), "remote endpoints must use HTTPS");
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

  const httpDraft = draftFromServer(
    serverView({
      config: {
        transport: "http",
        command: undefined,
        args: undefined,
        url: "https://mcp.example.com/mcp",
      },
    }).config,
  );
  assert.equal(httpDraft.url, "https://mcp.example.com/mcp");
  assert.deepEqual(Object.keys(toSaveInput(httpDraft)).sort(), [
    "enabled",
    "id",
    "label",
    "transport",
    "url",
  ]);

  const storedAuthDraft = draftFromServer(
    serverView({
      config: {
        transport: "http",
        command: undefined,
        args: undefined,
        url: "https://mcp.example.com/mcp",
        httpAuthHeaders: [
          {
            name: "Authorization",
            credentialRef: "keychain://mcp/http-auth",
          },
          {
            name: "X-Gateway-Key",
            credentialRef: "keychain://mcp/http-gateway",
          },
        ],
      },
    }).config,
  );
  assert.equal(storedAuthDraft.httpAuthHeaders, "Authorization:\nX-Gateway-Key:");
  assert.deepEqual(toSaveInput(storedAuthDraft).httpAuthHeaders, [
    { name: "Authorization" },
    { name: "X-Gateway-Key" },
  ]);

  const oauthDraft = draftFromServer(
    serverView({
      config: {
        transport: "http",
        command: undefined,
        args: undefined,
        url: "https://mcp.example.com/mcp",
        oauth: oauthConfig({ scopes: ["mcp.read", "mcp.tools"] }),
      },
    }).config,
  );
  assert.equal(oauthDraft.authMode, "oauth");
  assert.equal(oauthDraft.oauthIssuer, "https://auth.example.com");
  assert.equal(oauthDraft.oauthClientId, "desktop-client");
  assert.equal(oauthDraft.oauthFlow, "authorization_code");
  assert.equal(oauthDraft.oauthClientAuth, "none");
  assert.equal(oauthDraft.oauthDpop, false, "默认关着：多数服务器还不要发送方约束令牌");
  // 只写字段：表单从不回填 secret，因为服务端也不会把它送回来。
  assert.equal(oauthDraft.oauthClientSecret, "");
  assert.equal(oauthDraft.oauthScopes, "mcp.read\nmcp.tools");
  assert.deepEqual(toSaveInput(oauthDraft).oauth, {
    issuer: "https://auth.example.com",
    clientId: "desktop-client",
    flow: "authorization_code",
    clientAuth: "none",
    dpop: false,
    scopes: ["mcp.read", "mcp.tools"],
  });
  assert.deepEqual(toSaveInput({ ...oauthDraft, oauthIssuer: "" }).oauth, {
    issuer: "",
    clientId: "desktop-client",
    flow: "authorization_code",
    clientAuth: "none",
    dpop: false,
    scopes: ["mcp.read", "mcp.tools"],
  });
  assert.deepEqual(toSaveInput({ ...oauthDraft, oauthClientId: "" }).oauth, {
    issuer: "https://auth.example.com",
    clientId: "",
    flow: "authorization_code",
    clientAuth: "none",
    dpop: false,
    scopes: ["mcp.read", "mcp.tools"],
  });
  // 客户端密钥同样是搬运：填了就发，留空就整个字段都不出现（后端据此保留或回收）。
  assert.deepEqual(
    toSaveInput({ ...oauthDraft, oauthClientAuth: "client_secret_basic" }).oauth,
    {
      issuer: "https://auth.example.com",
      clientId: "desktop-client",
      flow: "authorization_code",
      clientAuth: "client_secret_basic",
      dpop: false,
      scopes: ["mcp.read", "mcp.tools"],
    },
  );
  assert.deepEqual(
    toSaveInput({
      ...oauthDraft,
      oauthClientAuth: "client_secret_post",
      oauthClientSecret: "s3cret",
    }).oauth,
    {
      issuer: "https://auth.example.com",
      clientId: "desktop-client",
      flow: "authorization_code",
      clientAuth: "client_secret_post",
      clientSecret: "s3cret",
      dpop: false,
      scopes: ["mcp.read", "mcp.tools"],
    },
  );
  // 流程是存下来的身份的一部分，表单只是把它搬运过去，不做任何推断。
  assert.equal(
    toSaveInput({ ...oauthDraft, oauthFlow: "device_code" }).oauth?.flow,
    "device_code",
  );

  const authenticatedDraft: McpServerDraft = {
    ...httpDraft,
    authMode: "static",
    httpAuthHeaders:
      " Authorization : Bearer test-secret\nX-API-Key: gateway:secret ",
  };
  const authenticatedInput = toSaveInput(authenticatedDraft);
  assert.deepEqual(authenticatedInput.httpAuthHeaders, [
    { name: "Authorization", secret: "Bearer test-secret" },
    { name: "X-API-Key", secret: "gateway:secret" },
  ]);

  const preservedInput = toSaveInput({
    ...authenticatedDraft,
    httpAuthHeaders: "X-API-Key:\nAuthorization:",
  });
  assert.deepEqual(preservedInput.httpAuthHeaders, [
    { name: "X-API-Key" },
    { name: "Authorization" },
  ]);
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

test("a running server exposes the explicit tool review surface", () => {
  const html = render({
    servers: [
      serverView({
        config: { allowedTools: ["echo"], trustLevel: "reviewed" },
        status: { running: true, pid: 4242, toolCount: 2 },
        tools: [
          { name: "echo", description: "echo", inputSchema: {} },
          { name: "explode", description: "boom", inputSchema: {} },
        ],
      }),
    ],
  });
  assert.match(html, /审核工具/);
  assert.match(html, /当前允许 1 个/);
  assert.match(html, /保存工具审核/);
  assert.match(html, /每次调用仍按 critical 逐项批准/);
  assert.match(html, /type="checkbox"/);
});

test("a crashed server shows bounded recovery progress and keeps the exit record visible", () => {
  const html = render({
    servers: [
      serverView({
        status: {
          lastExit: { code: 7, signal: null, at: "2025-01-01T00:00:00Z", reason: "exit code 7" },
          restart: { attempt: 1, maxAttempts: 3, exhausted: false, at: "2025-01-01T00:00:01Z" },
          stderrTail: ["failed to bind"],
        },
        unavailable: {
          code: "exited",
          message:
            'mcp server "mcp_1" exited (exit code 7) at 2025-01-01T00:00:00Z; automatic recovery attempt 1/3 is pending.',
        },
      }),
    ],
  });

  assert.match(html, /exit code 7/);
  assert.match(html, /正在恢复/);
  assert.match(html, /不重放刚才的调用/);
  assert.match(html, /failed to bind/);
  // The user can still stop/replace the row while recovery is pending.
  assert.ok(buttonNamed(html, "启动"));
  assert.ok(!/已恢复|恢复成功/.test(html), "a pending retry must not claim success");

  const exhausted = serverView({
    status: {
      lastExit: { code: 7, signal: null, at: "2025-01-01T00:00:00Z", reason: "exit code 7" },
      restart: { attempt: 3, maxAttempts: 3, exhausted: true, at: "2025-01-01T00:00:03Z" },
    },
  });
  assert.deepEqual(statusLabel(exhausted), { running: false, text: "恢复已停止" });
  assert.match(mcpRestartText(exhausted) ?? "", /3\/3/);
});

test("an invalid HTTP row is shown with the core's refusal and cannot be started", () => {
  const html = render({
    servers: [
      serverView({
        config: { id: "mcp_http", transport: "http", command: undefined, args: undefined, url: "https://mcp.example.com" },
        unavailable: {
          code: "invalid_config",
          message:
            'mcp server "mcp_http" has an invalid HTTP endpoint: the endpoint must not contain a URL fragment',
        },
      }),
    ],
  });

  assert.match(html, /invalid HTTP endpoint/);
  assert.match(html, /https:\/\/mcp\.example\.com/);
  assert.match(html, /http/);
  assert.ok(buttonNamed(html, "启动").disabled, "一个起不来的行不该摆出能点的「启动」");
  assert.ok(!/忽略|仍然继续|ignore/i.test(html), "没有「忽略并继续」这条路");
});

test("a valid HTTP row exposes its endpoint and can be started", () => {
  const html = render({
    servers: [
      serverView({
        config: {
          id: "mcp_http",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
        },
      }),
    ],
  });

  assert.match(html, /https:\/\/mcp\.example\.com\/mcp/);
  assert.equal(buttonNamed(html, "启动").disabled, false);
});

test("a disabled row cannot be started either, and says so in the title", () => {
  const html = render({ servers: [serverView({ config: { enabled: false } })] });
  const start = buttonNamed(html, "启动");
  assert.ok(start.disabled);
  assert.match(html, /被禁用/);
});

test("the form offers stdio and Streamable HTTP with transport-specific fields", () => {
  const html = render({ editingId: "mcp_1", draft: draftFromServer(serverView({}).config) });

  assert.match(html, /一行一个/);
  assert.match(html, /保存\*\*不会启动\*\*服务器|保存/);
  assert.match(html, /每个 MCP 工具调用都需要用户逐项批准/);
  assert.match(html, /<option[^>]*value="stdio"/);
  assert.match(html, /<option[^>]*value="http"/);
  assert.match(html, /启动命令/);
  assert.doesNotMatch(html, /Endpoint URL/);

  const httpHtml = render({
    editingId: "mcp_http",
    draft: draftFromServer(
      serverView({
        config: {
          id: "mcp_http",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          httpAuthHeaders: [
            {
              name: "Authorization",
              credentialRef: "keychain://mcp/mcp_http",
            },
            {
              name: "X-API-Key",
              credentialRef: "keychain://mcp/mcp_http-key",
            },
          ],
        },
      }).config,
    ),
  });
  assert.match(httpHtml, /Endpoint URL/);
  assert.match(httpHtml, /Authentication headers/);
  assert.match(httpHtml, /<textarea/);
  assert.match(httpHtml, /Authorization/);
  assert.match(httpHtml, /X-API-Key/);
  assert.match(httpHtml, /保留现有 secret/);
  assert.doesNotMatch(httpHtml, /启动命令/);

  const oauthHtml = render({
    editingId: "mcp_oauth",
    draft: draftFromServer(
      serverView({
        config: {
          id: "mcp_oauth",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig(),
        },
      }).config,
    ),
  });
  assert.match(oauthHtml, /OAuth 2\.1/);
  assert.match(oauthHtml, /OAuth issuer/);
  assert.match(oauthHtml, /OAuth client id/);
  assert.match(oauthHtml, /动态客户端注册/);
  assert.match(oauthHtml, /https:\/\/auth\.example\.com/);
  // 两种流程都在选择框里，默认是授权码；界面还用同一份值决定下面那句说明说什么。
  assert.match(oauthHtml, /<option[^>]*value="authorization_code"/);
  assert.match(oauthHtml, /<option[^>]*value="device_code"/);
  assert.match(oauthHtml, /随机|127\.0\.0\.1|等回调/);

  const deviceHtml = render({
    editingId: "mcp_oauth",
    draft: draftFromServer(
      serverView({
        config: {
          id: "mcp_oauth",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig({ flow: "device_code" }),
        },
      }).config,
    ),
  });
  assert.match(deviceHtml, /value="device_code" selected/);
  assert.match(deviceHtml, /user_code/);
  assert.match(deviceHtml, /device_authorization_endpoint/);
});

test("the OAuth form offers the client authentication methods and a write-only secret", () => {
  const publicHtml = render({
    editingId: "mcp_oauth",
    draft: draftFromServer(
      serverView({
        config: {
          id: "mcp_oauth",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig(),
        },
      }).config,
    ),
  });
  assert.match(publicHtml, /<option[^>]*value="none"/);
  assert.match(publicHtml, /<option[^>]*value="client_secret_post"/);
  assert.match(publicHtml, /<option[^>]*value="client_secret_basic"/);
  assert.match(publicHtml, /公共客户端/);
  assert.match(publicHtml, /服务端即使在注册响应里返回 secret/);
  assert.doesNotMatch(publicHtml, /Client secret/, "公共客户端没有密钥可填");

  const secretHtml = render({
    editingId: "mcp_oauth",
    draft: draftFromServer(
      serverView({
        config: {
          id: "mcp_oauth",
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig({ clientAuth: "client_secret_basic" }),
        },
      }).config,
    ),
  });
  assert.match(secretHtml, /value="client_secret_basic" selected/);
  assert.match(secretHtml, /Client secret/);
  assert.match(secretHtml, /type="password"/);
  assert.match(secretHtml, /留空保留现有 secret/);
  assert.match(secretHtml, /旧值会被回收/);
});

test("an OAuth row names its client authentication method", () => {
  const html = render({
    servers: [
      serverView({
        config: {
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig({ clientAuth: "client_secret_post" }),
        },
      }),
    ],
  });
  assert.match(html, /client_secret_post/);

  // 公共客户端不多说一句：默认状态不需要占位（否则每行都会多出同样的三个字）。
  const publicRow = render({
    servers: [
      serverView({
        config: {
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig(),
        },
      }),
    ],
  });
  assert.doesNotMatch(publicRow, /client_secret_post|client_secret_basic/);
});

test("only the secret methods ask for a stored client secret", () => {
  assert.equal(oauthClientAuthNeedsSecret("none"), false);
  assert.equal(oauthClientAuthNeedsSecret("client_secret_post"), true);
  assert.equal(oauthClientAuthNeedsSecret("client_secret_basic"), true);
});

test("an OAuth row exposes connect or re-authorize without implying it is connected", () => {
  const html = render({
    servers: [
      serverView({
        config: {
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig(),
        },
      }),
    ],
  });
  assert.match(html, /连接 OAuth/);
  assert.doesNotMatch(html, /重新授权/);
  assert.match(html, /OAuth 授权码/);

  const connected = render({
    servers: [
      serverView({
        config: {
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig({
            tokenEndpoint: "https://auth.example.com/token",
            credentialRef: "keychain://mcp/oauth",
          }),
        },
      }),
    ],
  });
  assert.match(connected, /重新授权/);

  // 设备码那一行也要在列表里说清它选的是哪种流程：两种流程用户要去的地方不一样。
  const deviceRow = render({
    servers: [
      serverView({
        config: {
          transport: "http",
          command: undefined,
          args: undefined,
          url: "https://mcp.example.com/mcp",
          oauth: oauthConfig({ flow: "device_code" }),
        },
      }),
    ],
  });
  assert.match(deviceRow, /OAuth 设备码/);
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

/* ── P0-1：设备码（RFC 8628） ──────────────────────────────────────────────── */

function devicePrompt(
  overrides: Partial<McpOAuthDeviceCodeEvent> = {},
): McpOAuthDeviceCodeEvent {
  return {
    serverId: "mcp_oauth",
    userCode: "WDJB-MJHT",
    verificationUri: "https://auth.example.com/device",
    verificationUriComplete: "https://auth.example.com/device?user_code=WDJB-MJHT",
    expiresAt: "2026-01-01T00:05:00Z",
    ...overrides,
  };
}

const deviceNow = Date.parse("2026-01-01T00:00:00Z");

function renderDevice(
  phase: McpOAuthDeviceCodePhase,
  options: {
    message?: string | null;
    overrides?: Partial<McpOAuthDeviceCodeEvent>;
    now?: number;
  } = {},
): string {
  return renderToStaticMarkup(
    <McpOAuthDeviceCodeView
      prompt={devicePrompt(options.overrides)}
      phase={phase}
      message={options.message ?? null}
      now={options.now ?? deviceNow}
      onOpenLink={noop}
      onCopyCode={noop}
      onCancel={noop}
      onDismiss={noop}
    />,
  );
}

test("the device dialog shows the code, where to type it, and the wait", () => {
  const waiting = renderDevice("waiting");

  // The code is the one string the user has to carry to another device, so it is shown
  // verbatim — same spelling, same case, no reformatting.
  assert.match(waiting, /<code>WDJB-MJHT<\/code>/);
  assert.match(waiting, /https:\/\/auth\.example\.com\/device/);
  assert.match(waiting, /等待授权/);
  assert.match(waiting, /还没有收到授权结果/);
  assert.match(waiting, /5 分钟后过期/);
  assert.ok(buttonNamed(waiting, "取消授权"), "等待中可以取消");
  assert.ok(buttonNamed(waiting, "复制代码"));
  assert.ok(!/已结束/.test(waiting), "等待中的流程不能被说成结束了");
  assert.ok(!/role="alert"/.test(waiting), "等待不是错误");
});

test("the device dialog's terminal states say what happened without inventing a reason", () => {
  const succeeded = renderDevice("succeeded");
  assert.match(succeeded, /role="status"/);
  assert.match(succeeded, /已结束/);
  assert.ok(buttonNamed(succeeded, "关闭"));
  assert.ok(!buttons(succeeded).some((button) => button.text.includes("取消授权")));

  // Failure carries the backend's own words: `access_denied` and `expired_token` are two
  // different things to do next, and a rewrite here would blur them.
  const denied = renderDevice("failed", {
    message: "OAuth device authorization was denied by the user (access_denied)",
  });
  assert.match(denied, /role="alert"/);
  assert.match(denied, /access_denied/);

  const expired = renderDevice("failed", {
    message: "OAuth device code expired before it was approved (expired_token)",
  });
  assert.match(expired, /expired_token/);

  // A cancel the user asked for is not an error and must not be announced as one.
  const cancelled = renderDevice("cancelled");
  assert.match(cancelled, /role="status"/);
  assert.ok(!/role="alert"/.test(cancelled));
  assert.match(cancelled, /已取消/);
});

test("the verification link prefers the complete URI and falls back to the plain one", () => {
  assert.equal(
    deviceCodeLink(devicePrompt()),
    "https://auth.example.com/device?user_code=WDJB-MJHT",
  );
  assert.equal(
    deviceCodeLink(devicePrompt({ verificationUriComplete: undefined })),
    "https://auth.example.com/device",
  );
  // Both are on screen, so a browser that does not prefill still leaves the user able to
  // finish: the plain URL is rendered as text next to the button.
  const html = renderDevice("waiting", { overrides: { verificationUriComplete: undefined } });
  assert.match(html, /https:\/\/auth\.example\.com\/device/);
});

test("the expiry line is computed from the code's own deadline", () => {
  assert.equal(
    deviceCodeExpiryText(devicePrompt(), deviceNow),
    "验证码大约在 5 分钟后过期。",
  );
  assert.equal(
    deviceCodeExpiryText(devicePrompt(), Date.parse("2026-01-01T00:04:30Z")),
    "验证码大约在 1 分钟后过期。",
  );
  assert.match(
    deviceCodeExpiryText(devicePrompt(), Date.parse("2026-01-01T00:06:00Z")) ?? "",
    /已过期/,
  );
  // An unparsable deadline says nothing rather than guessing: a wrong countdown is worse
  // than no countdown.
  assert.equal(deviceCodeExpiryText(devicePrompt({ expiresAt: "soon" }), deviceNow), null);
});
