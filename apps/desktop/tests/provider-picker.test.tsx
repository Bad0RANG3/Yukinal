/**
 * 选 Provider 的那个选择框。
 *
 * 它替换掉的是一张「每个 Provider 一行」的列表 —— 十四个 Provider 就是十四行，而这一屏
 * 要回答的只有「我在看哪一个」「新建运行会用哪一个」。所以这里钉住的是**结构**：
 *
 * - 全部 Provider 都在一个 `<select>` 里，而不是铺成一段可无限增长的列表；
 * - 名称重复时必须带上 id —— 两个一模一样的选项等于让人猜（真实数据里就有四个同名）；
 * - 「当前使用」只出现在启用的那一个上，且它的动作按钮是禁用的（没有第二个可点的动作）；
 * - 切选择框**不**等于切换启用的 Provider，那是另一个按钮的事，且要说清副作用。
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import type { AiProviderConfig } from "@yukinal/shared";

import { NEW_PROVIDER_VALUE, ProviderPicker } from "../src/features/settings/ProviderPicker.js";
import { providerDeleteNotice, providerHost, providerOptionLabel } from "../src/lib/providers.js";

function provider(overrides: Partial<AiProviderConfig> & { id: string }): AiProviderConfig {
  return {
    kind: "openai-compatible",
    label: overrides.id,
    baseUrl: "https://gw.example.com/v1",
    model: "deepseek-v4-flash",
    enabled: false,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

const noop = () => {};

function render(overrides: Partial<Parameters<typeof ProviderPicker>[0]> = {}): string {
  return renderToStaticMarkup(
    <ProviderPicker
      providers={[provider({ id: "deepseek", label: "deepseek", enabled: true })]}
      viewingId="deepseek"
      busy={false}
      confirmingId={null}
      onView={noop}
      onActivate={noop}
      onRequestDelete={noop}
      onCancelDelete={noop}
      onConfirmDelete={noop}
      {...overrides}
    />,
  );
}

interface RenderedButton {
  text: string;
  disabled: boolean;
  title: string;
}

/** 按按钮切分再断言，避免「页面上别处也有 disabled」这种假通过。 */
function buttons(html: string): RenderedButton[] {
  return [...html.matchAll(/<button([^>]*)>([\s\S]*?)<\/button>/g)].map((match) => ({
    text: (match[2] ?? "").replace(/<[^>]*>/g, "").replace(/\s+/g, ""),
    disabled: /\bdisabled\b/.test(match[1] ?? ""),
    title: /title="([^"]*)"/.exec(match[1] ?? "")?.[1] ?? "",
  }));
}

/* ── 纯规则 ───────────────────────────────────────────────────────────────── */

test("a duplicate name is told apart by its gateway host, not by its id", () => {
  const providers = [
    provider({ id: "deepseek", label: "deepseek", baseUrl: "https://api.deepseek.com/v1" }),
    provider({
      id: "prv_ccswitch_codex_5feb2e96_07af_43bb_b33c_2a1c1a4b79dc_b30acd2fb43fba84",
      label: "deepseek",
      baseUrl: "https://ccswitch.internal.example/gateway/v1",
    }),
    provider({ id: "zhipu_glm", label: "Zhipu GLM", baseUrl: "https://open.bigmodel.cn/api/paas/v4" }),
  ];

  // 不同网关：host 就是那条最短的区别，而且它正是这些同名条目真正不一样的地方。
  assert.equal(
    providerOptionLabel(providers[0]!, providers),
    "deepseek · deepseek-v4-flash · api.deepseek.com",
  );
  assert.equal(
    providerOptionLabel(providers[1]!, providers),
    "deepseek · deepseek-v4-flash · ccswitch.internal.example",
  );
  // 不重名的一个字都不多背。
  assert.equal(providerOptionLabel(providers[2]!, providers), "Zhipu GLM · deepseek-v4-flash");
});

test("the same name and model on the same gateway falls back to an id tail, not to the whole id", () => {
  // 真实数据里就是这样四行：名称、模型、网关、密钥引用全部一样，只有 id 不同。
  const providers = [
    provider({ id: "prv_1a085ad17fab", label: "deepseek" }),
    provider({ id: "prv_1a08a8e71bd3", label: "deepseek" }),
    provider({ id: "prv_ccswitch_codex_5feb2e96_07af_43bb_b33c_2a1c1a4b79dc_b30acd2fb43fba84", label: "deepseek" }),
  ];

  assert.deepEqual(
    providers.map((item) => providerOptionLabel(item, providers)),
    [
      "deepseek · deepseek-v4-flash · …d17fab",
      "deepseek · deepseek-v4-flash · …e71bd3",
      "deepseek · deepseek-v4-flash · …3fba84",
    ],
  );
  for (const label of providers.map((item) => providerOptionLabel(item, providers))) {
    assert.ok(label.length < 50, `下拉里的选项不能变成一团乱码：${label}`);
  }

  // 尾部也撞上时退回完整 id：宁可长，不可歧义。
  const colliding = [
    provider({ id: "prv_aaaaaaaaaa111111", label: "deepseek" }),
    provider({ id: "prv_bbbbbbbbbb111111", label: "deepseek" }),
  ];
  assert.ok(providerOptionLabel(colliding[0]!, colliding).endsWith("prv_aaaaaaaaaa111111"));
});

test("a different model already separates two same-named providers, so nothing is appended", () => {
  const providers = [
    provider({ id: "prv_a", label: "custom", model: "deepseek-v4-flash" }),
    provider({ id: "prv_b", label: "custom", model: "gpt-5.6-terra" }),
  ];
  assert.deepEqual(
    providers.map((item) => providerOptionLabel(item, providers)),
    ["custom · deepseek-v4-flash", "custom · gpt-5.6-terra"],
  );
});

test("the host line never carries a path or credentials", () => {
  // schema 本来就禁止内嵌凭据，这里钉的是「即使有也不会因为这一行漏出去」。
  assert.equal(providerHost("https://user:secret@api.example.com/v1/models?key=abc"), "api.example.com");
  assert.equal(providerHost("https://api.example.com:8443/v1"), "api.example.com:8443");
  assert.equal(providerHost("不是地址"), null, "解析不出来时界面不该显示半个地址");

  const providers = [
    provider({ id: "prv_a", label: "同名", baseUrl: "不是地址" }),
    provider({ id: "prv_b", label: "同名", baseUrl: "也不是地址" }),
  ];
  for (const item of providers) {
    const label = providerOptionLabel(item, providers);
    assert.ok(!label.includes("不是地址"), label);
  }
});

test("a provider with no model still reads as one line, not as a dangling separator", () => {
  const bare = provider({ id: "bare", label: "Bare", model: "" });
  assert.equal(providerOptionLabel(bare, [bare]), "Bare");
});

/* ── 渲染 ─────────────────────────────────────────────────────────────────── */

test("every provider is offered in one select instead of one row each", () => {
  const providers = [
    provider({ id: "deepseek", label: "deepseek", enabled: true }),
    provider({ id: "openai", label: "OpenAI" }),
    provider({ id: "gemini", label: "Gemini", kind: "gemini" }),
  ];
  const html = render({ providers, viewingId: "openai" });

  assert.equal([...html.matchAll(/<select\b/g)].length, 1, "只该有一个选择框");
  for (const item of providers) {
    assert.match(html, new RegExp(`<option[^>]*value="${item.id}"`), `${item.id} 必须在下拉里`);
  }
  assert.match(html, /<option[^>]*value=""/, "新建也是一个取值");
  // 列表那一套（每行一个「编辑」按钮）不该再出现。
  assert.equal([...html.matchAll(/<option\b/g)].length, providers.length + 1);
  assert.ok(!/编辑/.test(html), "选择框取代了逐行的「编辑」按钮");
});

test("the enabled provider is the one marked, and only it", () => {
  const providers = [
    provider({ id: "deepseek", label: "deepseek", enabled: true }),
    provider({ id: "openai", label: "OpenAI" }),
  ];
  const html = render({ providers, viewingId: "openai" });

  const marked = [...html.matchAll(/<option[^>]*>([\s\S]*?)<\/option>/g)]
    .map((match) => (match[1] ?? "").replace(/<[^>]*>/g, ""))
    .filter((text) => text.includes("当前使用"));
  assert.deepEqual(marked, ["deepseek · deepseek-v4-flash · 当前使用"]);

  // 正在看的是一个未启用的 Provider：动作可用，而且说清「其余会停用」。
  const activate = buttons(html).find((button) => button.text === "设为当前");
  assert.ok(activate, "查看未启用的 Provider 时要能把它设为当前");
  assert.equal(activate.disabled, false);
  assert.match(activate.title, /其余 Provider 会被停用/);
  assert.match(html, /未启用：配置保留/);
});

test("the already-current provider has no second action to press", () => {
  const html = render();
  const current = buttons(html).find((button) => button.text === "当前使用中");
  assert.ok(current, "当前那个要显示成状态，而不是一个点了没反应的按钮");
  assert.equal(current.disabled, true);
  assert.match(html, /新建 Agent 运行会使用它/);
});

test("the new-provider sentinel shows the empty form's consequence, not a provider's", () => {
  const html = render({ viewingId: NEW_PROVIDER_VALUE });

  assert.match(html, /<option[^>]*value="" selected/);
  assert.equal(buttons(html).length, 0, "新建时没有「设为当前」可点");
  assert.match(html, /保存后会启用它，其余 Provider 自动停用/);
});

test("a busy write disables the control instead of letting two writes race", () => {
  const html = render({ busy: true });
  assert.match(html, /<select[^>]*disabled/);
  assert.equal(buttons(html)[0]?.disabled, true);
});

/* ── 删除 ─────────────────────────────────────────────────────────────────── */

test("the provider being viewed can be deleted, and the button asks before it does", () => {
  const html = render();
  assert.deepEqual(buttons(html).map((button) => button.text), ["当前使用中", "删除"]);
  assert.ok(!/provider-picker-confirm"/.test(html), "点「删除」之前没有确认块");
});

test("the confirmation names the provider and says what happens to the key", () => {
  const html = render({
    providers: [provider({ id: "deepseek", label: "deepseek", enabled: true, apiKeyCredentialRef: "keychain://openai/deepseek" })],
    confirmingId: "deepseek",
  });

  assert.match(html, /删除「deepseek」？/);
  assert.match(html, /没有别的 Provider 引用同一份密钥时，密钥也会从系统密钥链移除/);
  // 确认块取代了两个动作：不能在「正在确认」的同时又点得动「设为当前」。
  assert.equal(
    buttons(html).filter((button) => button.text === "设为当前" || button.text === "当前使用中").length,
    0,
  );
  const actions = buttons(html).map((button) => button.text);
  assert.deepEqual(actions, ["取消", "删除"]);
  assert.match(html, /<button[^>]*autofocus[^>]*>取消<\/button>/, "回车不该默认落在「删除」上");
});

test("a provider with no key is never told a key will be removed", () => {
  // 本地端点、免鉴权的网关本来就没有引用；对着它们说「密钥也会被删」是凭空吓人。
  const html = render({ confirmingId: "deepseek" });
  assert.ok(!/密钥也会从系统密钥链移除/.test(html), html);
  assert.match(html, /删除「deepseek」？这份配置会消失。/);
});

test("deleting the provider that runs are using warns about who takes over", () => {
  const enabled = render({
    providers: [provider({ id: "deepseek", label: "deepseek", enabled: true, apiKeyCredentialRef: "keychain://openai/deepseek" })],
    confirmingId: "deepseek",
  });
  assert.match(enabled, /它当前是新建运行使用的 Provider/);
  assert.match(enabled, /最近更新的那个/);

  const idle = render({
    providers: [provider({ id: "deepseek", label: "deepseek", apiKeyCredentialRef: "keychain://openai/deepseek" })],
    confirmingId: "deepseek",
  });
  assert.ok(!/最近更新的那个/.test(idle), "没启用的那一个不该吓唬人");
});

/* ── 删除之后那句话 ───────────────────────────────────────────────────────── */

test("the delete notice tells the three outcomes apart instead of guessing", () => {
  const withKey = provider({ id: "prv_a", label: "deepseek", apiKeyCredentialRef: "keychain://openai/deepseek" });
  assert.equal(
    providerDeleteNotice(withKey, true),
    "已删除「deepseek」，那份密钥也已从系统密钥链移除。",
  );
  // 引用被别的行共用：密钥留着，而且这句话是这次操作唯一能说清它的地方。
  assert.equal(
    providerDeleteNotice(withKey, false),
    "已删除「deepseek」；那份密钥仍被别的 Provider 使用，没有动它。",
  );
  // 没有引用的行（本地端点）也回 `credentialReclaimed: false` —— 第三种情况不能沿用第二句。
  const noKey = provider({ id: "prv_b", label: "OpenAI", apiKeyCredentialRef: undefined });
  assert.equal(providerDeleteNotice(noKey, false), "已删除「OpenAI」。这份配置本来就没有密钥。");
  assert.ok(!/仍被别的 Provider 使用/.test(providerDeleteNotice(noKey, false)));
});

test("a busy delete says so instead of looking like a click that did nothing", () => {
  const html = render({ confirmingId: "deepseek", busy: true });
  const confirm = buttons(html);
  assert.equal(confirm.find((button) => button.text === "删除中…")?.disabled, true);
  // 「取消」不是一次写入：请求已经在飞了，它只是关掉这块 UI。
  assert.equal(confirm.find((button) => button.text === "取消")?.disabled, false);
});

/**
 * 二次确认的两条实现约定：用行内确认而不是 `window.confirm`（系统弹窗盖住上下文，也说不清
 * 密钥会不会一起删），以及删除请求只有一处发起。
 */
test("the delete path uses an inline confirmation and one command", () => {
  const picker = readFileSync(new URL("../src/features/settings/ProviderPicker.tsx", import.meta.url), "utf8");
  assert.ok(!/window\.confirm/.test(picker), "不用系统确认框");

  const settings = readFileSync(new URL("../src/features/settings/RuntimeSettings.tsx", import.meta.url), "utf8");
  assert.ok(!/window\.confirm/.test(settings), "不用系统确认框");
  assert.equal([...settings.matchAll(/IPC_COMMANDS\.providerDelete/g)].length, 1);
  // 「密钥还在不在」只有这一次响应知道：那句话由纯函数给出，三种结局各一句。
  assert.match(settings, /providerDeleteNotice\(provider, credentialReclaimed\)/);
  assert.match(settings, /confirmingId=\{confirmingId\}/);
  const providers = readFileSync(new URL("../src/lib/providers.ts", import.meta.url), "utf8");
  assert.match(providers, /已从系统密钥链移除/);
  assert.match(providers, /仍被别的 Provider 使用/);
  assert.match(providers, /本来就没有密钥/);
});

/**
 * 结构上的那条要求只能从源码上钉：列表一旦被重新写回来，上面这些断言照样会通过
 * （选择框还在），而这一屏又会长回十四行。
 */
test("the settings page renders the picker, not a per-provider row list", () => {
  const source = readFileSync(new URL("../src/features/settings/RuntimeSettings.tsx", import.meta.url), "utf8");
  assert.match(source, /<ProviderPicker/);
  assert.ok(!/provider-list|provider-row\b/.test(source), "Provider 不该再铺成一串行");
});
