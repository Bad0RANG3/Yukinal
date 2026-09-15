/**
 * 主机指纹面板（ADR 0012）：把「不一致时到底显示了什么」钉成断言。
 *
 * 这个功能里最要紧的几件事都是**文案与结构**，不是数据流：
 *
 * - 不匹配时两个指纹都要出现在屏幕上（只显示一个，用户没有判断依据）；
 * - 探针结果不能被称为「已验证」（它只是服务器的主张）；
 * - 不匹配时**没有**任何可以一次点击就接受新指纹的按钮（第 5 条：没有「仍然继续」）；
 * - 「信任此指纹」只在这一次会话真的探到过那个指纹之后才可用；
 * - 「遗忘」旁边必须写明下一次连接会回到 TOFU。
 *
 * 右边这些用 Rust 或 zod 都测不到（它们在渲染里），所以这里直接渲染静态 HTML 断言。
 * 用 `renderToStaticMarkup` 与 `panel-states.test.tsx` 一致：没有 DOM，所以不需要
 * jsdom，而这几条断言要的本来就是「最终生成了什么标记」。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { HostKeyPanel } from "../src/features/servers/HostKeySection.js";
import { hostKeyTrustEligibility } from "../src/lib/host-key.js";

const PINNED = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
const PRESENTED = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

const noop = () => {};

interface RenderedButton {
  text: string;
  disabled: boolean;
}

/**
 * 把渲染出来的按钮摘出来（去掉里面的 svg，留文本）。
 *
 * 直接用字符串断言 `disabled` 会踩到「页面上别的地方也有 disabled」这种假通过：
 * 这里要断言的是**某一个**按钮的可用性，所以必须按按钮切分。
 */
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

const status = (pinnedFingerprint?: string) => ({
  host: "api.example.com",
  port: 22,
  pinned: pinnedFingerprint !== undefined,
  ...(pinnedFingerprint === undefined ? {} : { pinnedFingerprint }),
});

/* ── 未核验 ────────────────────────────────────────────────────────────────── */

test("an unpinned host says 未核验 instead of showing an empty fingerprint", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel status={status()} onProbe={noop} onTrust={noop} onForget={noop} />,
  );

  assert.match(html, /未核验/);
  // 没有钉子就不该画出指纹框：一个空框看起来像「指纹是空的」。
  assert.doesNotMatch(html, /host-key-fingerprint/);
  // 没有钉子就没有「遗忘」：没有东西可遗忘。
  assert.equal(buttons(html).some((button) => button.text.includes("遗忘")), false);
});

test("trust is disabled until a probe produced a fingerprint in this session", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel status={status()} onProbe={noop} onTrust={noop} onForget={noop} />,
  );

  const trust = buttonNamed(html, "信任此指纹");
  assert.equal(trust.disabled, true, "没探针就不能信任");
  assert.match(html, /先点「探针」/, "禁用要说明原因，而不是只把按钮变灰");
});

/* ── 探针之后 ──────────────────────────────────────────────────────────────── */

test("a probe result is labelled as what the server presented, never as verified", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      status={status()}
      probe={{
        host: "api.example.com",
        port: 22,
        presentedFingerprint: PRESENTED,
        comparison: "unpinned",
      }}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  assert.match(html, /服务器出示的指纹/);
  assert.ok(html.includes(PRESENTED));
  assert.match(html, /这是它自己说的，尚未被核验/);
  // 「已验证」这四个字在这个面板里永远不该出现 —— 出示不等于核验。
  assert.doesNotMatch(html, /已验证/);
  assert.equal(buttonNamed(html, "信任此指纹").disabled, false, "探到了就应该可以钉住");
});

/* ── 不一致：这个功能存在的原因 ─────────────────────────────────────────────── */

test("a mismatch shows both fingerprints and offers no way to accept the new one", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      status={status(PINNED)}
      probe={{
        host: "api.example.com",
        port: 22,
        presentedFingerprint: PRESENTED,
        comparison: "mismatch",
        pinnedFingerprint: PINNED,
      }}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  // 两个指纹都在：只说「不一致」而不给两个值，用户没法判断。
  assert.ok(html.includes(PRESENTED), "出示的指纹必须在");
  assert.ok(html.includes(PINNED), "钉住的指纹也必须在");
  assert.match(html, /不是同一把/);

  // 没有「忽略 / 仍然继续 / 继续连接」这类按钮（ADR 0012 第 5 条）。
  const labels = buttons(html).map((button) => button.text).join(" ");
  assert.doesNotMatch(labels, /忽略/);
  assert.doesNotMatch(labels, /仍然继续/);
  assert.doesNotMatch(labels, /继续连接/);

  assert.equal(buttonNamed(html, "信任此指纹").disabled, true, "不一致时不能钉住新指纹");
  assert.equal(buttonNamed(html, "遗忘").disabled, false, "出路是遗忘，所以它必须可用");
  assert.match(html, /先「遗忘」/, "要告诉用户下一步");
});

test("a mismatch is announced, not just coloured", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      status={status(PINNED)}
      probe={{
        host: "api.example.com",
        port: 22,
        presentedFingerprint: PRESENTED,
        comparison: "mismatch",
        pinnedFingerprint: PINNED,
      }}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  // 颜色变化对读屏用户不存在，所以这一块必须是 alert。
  assert.match(html, /role="alert"/);
  assert.match(html, /可能是有人正在拦截这次连接/);
});

/* ── 已核验 ────────────────────────────────────────────────────────────────── */

test("a matching probe says so and leaves nothing to trust", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      status={status(PINNED)}
      probe={{
        host: "api.example.com",
        port: 22,
        presentedFingerprint: PINNED,
        comparison: "matches",
        pinnedFingerprint: PINNED,
      }}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  assert.match(html, /与已钉住的完全一致/);
  assert.equal(
    buttonNamed(html, "信任此指纹").disabled,
    true,
    "同一个指纹已经钉住了，重复钉没有意义",
  );

  // 遗忘的后果必须写在按钮旁边：它把下一次连接降级成不核验。
  assert.match(html, /下一次连接这台主机将不再核验/);
  assert.match(html, /TOFU/);
});

/* ── CA 策略 ───────────────────────────────────────────────────────────────── */

test("CA policy makes leaf pins visibly informational and disables pin actions", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      caPolicyEnabled
      status={status(PINNED)}
      probe={{
        host: "api.example.com",
        port: 22,
        presentedFingerprint: PRESENTED,
        comparison: "mismatch",
        pinnedFingerprint: PINNED,
      }}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  assert.match(html, /CA 策略已启用/);
  assert.match(html, /不参与校验/);
  assert.match(html, /本机保存的叶子指纹/);
  assert.equal(buttonNamed(html, "探针").disabled, false, "仍可查看服务器出示的叶子指纹");
  assert.equal(buttonNamed(html, "信任此指纹").disabled, true, "CA 模式下不能钉叶子指纹");
  assert.equal(buttonNamed(html, "遗忘").disabled, true, "CA 模式下不能遗忘叶子钉子");

  // 叶子 pin 被忽略时，mismatch 不能继续宣称连接会因此被拒绝。
  assert.doesNotMatch(html, /不是同一把/);
  assert.doesNotMatch(html, /任何连接都会继续被拒绝/);
  assert.doesNotMatch(html, /role="alert"/);
  assert.doesNotMatch(html, /连接时以它为准/);
});

test("CA policy without a saved leaf says so without calling the host unverified", () => {
  const html = renderToStaticMarkup(
    <HostKeyPanel
      caPolicyEnabled
      status={status()}
      onProbe={noop}
      onTrust={noop}
      onForget={noop}
    />,
  );

  assert.match(html, /未保存叶子指纹/);
  assert.doesNotMatch(html, /未核验/);
});

/* ── 规则本身 ──────────────────────────────────────────────────────────────── */

test("hostKeyTrustEligibility refuses every path that is not 'just probed'", () => {
  // 没探针：不可用。
  assert.deepEqual(hostKeyTrustEligibility(null), { trustable: false, reason: "no-probe" });
  assert.deepEqual(hostKeyTrustEligibility(undefined), { trustable: false, reason: "no-probe" });

  // 探到了、本来没钉子：可用，而且钉的就是探到的那个指纹。
  assert.deepEqual(
    hostKeyTrustEligibility({ presentedFingerprint: PRESENTED }),
    { trustable: true, fingerprint: PRESENTED },
  );

  // 探到了、钉子就是它：无事可做。
  assert.deepEqual(
    hostKeyTrustEligibility({ presentedFingerprint: PINNED, pinnedFingerprint: PINNED }),
    { trustable: false, reason: "already-pinned" },
  );

  // 探到了、钉着别的：**不可用** —— 这就是「不提供不一致时仍然继续」。
  assert.deepEqual(
    hostKeyTrustEligibility({ presentedFingerprint: PRESENTED, pinnedFingerprint: PINNED }),
    { trustable: false, reason: "mismatch" },
  );
});
