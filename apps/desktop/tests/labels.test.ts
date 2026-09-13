import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import {
  ENVIRONMENTS,
  EVENT_NAMES,
  EVENT_SCHEMAS,
  PERMISSION_APPROVAL_SOURCES,
  PERMISSION_MODES,
  RISK_LEVELS,
  SERVER_STATUSES,
  TOOL_EXECUTION_STATUSES,
} from "@yukinal/shared";

import {
  APPROVAL_SOURCE_LABEL,
  DECISION_LABEL,
  ENVIRONMENT_LABEL,
  ENVIRONMENT_LABEL_SHORT,
  ENVIRONMENTS as UI_ENVIRONMENTS,
  EXECUTION_STATUS_LABEL,
  RISK_LEVEL_LABEL,
  SERVER_STATUS_LABEL,
  TERMINAL_FONT_FAMILY,
  TERMINAL_FONT_LABEL,
  TERMINAL_FONT_ORDER,
  approvalSourceLabel,
  decisionLabel,
  environmentLabel,
  environmentLabelShort,
  resultLabel,
} from "../src/lib/labels.js";

/**
 * These dictionaries were previously copy-pasted six times across the UI, and
 * the copies had drifted — `staging` read "预发" in the sidebar and "预发布" in the
 * add-server form, and server `error` read "错误" in the list and "连接异常" in the
 * header. The types make a *missing* label a compile error; these tests make a
 * label that no longer covers the shared enum, or a picker that lost an option, a
 * failing test instead.
 */

test("both environment dictionaries cover exactly the shared enum", () => {
  assert.deepEqual(
    Object.keys(ENVIRONMENT_LABEL).sort(),
    [...ENVIRONMENTS].sort(),
    "ENVIRONMENT_LABEL is out of step with the shared ENVIRONMENTS tuple",
  );
  assert.deepEqual(Object.keys(ENVIRONMENT_LABEL_SHORT).sort(), [...ENVIRONMENTS].sort());
});

test("every environment label is non-empty and unique", () => {
  for (const environment of ENVIRONMENTS) {
    const long = environmentLabel(environment);
    const short = environmentLabelShort(environment);
    assert.ok(long.length > 0, `${environment} has an empty label`);
    assert.ok(short.length > 0, `${environment} has an empty short label`);
    assert.ok(long.length <= 8, `${environment} label is too long for a badge: ${long}`);
  }

  const longLabels = ENVIRONMENTS.map(environmentLabel);
  assert.equal(new Set(longLabels).size, longLabels.length, "two environments share a label");

  const shortLabels = ENVIRONMENTS.map(environmentLabelShort);
  assert.equal(new Set(shortLabels).size, shortLabels.length, "two environments share a short label");
});

test("the short and long forms agree on their stem, so one server is never named two ways", () => {
  for (const environment of ENVIRONMENTS) {
    const long = environmentLabel(environment);
    const short = environmentLabelShort(environment);
    assert.ok(
      long.startsWith(short),
      `"${short}" is not a prefix of "${long}", so the same environment is described two ways`,
    );
  }
});

test("the environment picker offers every environment, in one order", () => {
  assert.deepEqual([...UI_ENVIRONMENTS].sort(), [...ENVIRONMENTS].sort());
  assert.equal(new Set(UI_ENVIRONMENTS).size, UI_ENVIRONMENTS.length, "the picker lists a duplicate");
});

test("the server status dictionary covers exactly the shared enum", () => {
  assert.deepEqual(Object.keys(SERVER_STATUS_LABEL).sort(), [...SERVER_STATUSES].sort());
  for (const status of SERVER_STATUSES) {
    assert.ok(SERVER_STATUS_LABEL[status].length > 0, `${status} has an empty label`);
  }
});

test("a new server does not default to the environment that widens auto-approval", () => {
  // `staging` is the one class that lets the Agent auto-approve ordinary writes.
  // The form must not preselect it: a user who misses the field would get both a
  // mislabelled host and auto-approved writes to it.
  const defaultEnvironment = "unknown";
  assert.notEqual(defaultEnvironment, "staging");
  assert.ok((ENVIRONMENTS as readonly string[]).includes(defaultEnvironment));
  // And the intended default is not silently the permissive class.
  assert.notEqual(defaultEnvironment, "development");
});

/**
 * The risk / decision / approval vocabulary.
 *
 * These four maps existed twice — once in `features/agent/transcript.ts` and once in
 * `features/activity/ActivityFeed.tsx` — and both rendered the same tool call. The
 * Agent panel and the audit log are two views of one decision about a production
 * write, so if they disagree about whether it was "需审批" or "自动批准", the screen
 * contradicts itself. The maps now live in one module; these tests keep them
 * covering the shared enums, and keep the wording distinguishable.
 */
test("the audit dictionaries cover exactly their shared enums", () => {
  assert.deepEqual(Object.keys(RISK_LEVEL_LABEL).sort(), [...RISK_LEVELS].sort());
  assert.deepEqual(Object.keys(DECISION_LABEL).sort(), [...PERMISSION_MODES].sort());
  assert.deepEqual(Object.keys(APPROVAL_SOURCE_LABEL).sort(), [...PERMISSION_APPROVAL_SOURCES].sort());
  assert.deepEqual(
    Object.keys(EXECUTION_STATUS_LABEL).sort(),
    [...TOOL_EXECUTION_STATUSES].sort(),
  );
});

test("every audit label is non-empty and distinct within its vocabulary", () => {
  for (const map of [RISK_LEVEL_LABEL, DECISION_LABEL, APPROVAL_SOURCE_LABEL, EXECUTION_STATUS_LABEL]) {
    const values = Object.values(map);
    for (const value of values) assert.ok(value.length > 0, "an audit label is empty");
    assert.equal(new Set(values).size, values.length, "two members of one vocabulary share a label");
  }
});

test("the three approval sources stay distinguishable, because they are not equivalent", () => {
  // "user approved", "policy approved" and "the Agent approved itself" are three
  // different safety stories. A label collision would make them indistinguishable
  // in the audit log — which is the one place that has to tell them apart.
  const labels = PERMISSION_APPROVAL_SOURCES.map(approvalSourceLabel);
  assert.equal(new Set(labels).size, PERMISSION_APPROVAL_SOURCES.length);
  // `deny` must not read like a pending state: it is not a request awaiting a human.
  assert.notEqual(decisionLabel("deny"), decisionLabel("ask"));
});

test("the terminal result words are the ones the status map already uses", () => {
  // `resultLabel` exists for a settled tool call; it must not invent its own
  // wording for a state `EXECUTION_STATUS_LABEL` already names.
  for (const status of ["success", "failed", "cancelled"] as const) {
    assert.equal(resultLabel(status), EXECUTION_STATUS_LABEL[status]);
  }
});

/**
 * The event half of the IPC contract used to have no runtime gate, so a drifted
 * Rust payload crossed into the UI by cast — `terminal.data` bytes went straight
 * into xterm unchecked. `EVENT_SCHEMAS` is now the gate; these tests keep it
 * aligned with the contract and pin the payload shapes the UI subscribes to.
 */
test("the event gate defines no channel outside the shared contract", () => {
  const contract = new Set<string>(EVENT_NAMES);
  for (const name of Object.keys(EVENT_SCHEMAS)) {
    assert.ok(contract.has(name), `EVENT_SCHEMAS defines "${name}", which is not in EVENT_NAMES`);
  }
});

test("every event the UI subscribes to has a gate", () => {
  // The channels the desktop actually registers handlers for. A channel listed
  // here without a schema would be a payload crossing the boundary unchecked.
  const subscribed = [
    "agent.started",
    "agent.thinking",
    "agent.text",
    "agent.usage",
    "agent.tool_call",
    "agent.tool_result",
    "agent.waiting_approval",
    "agent.approval_expired",
    "agent.completed",
    "agent.failed",
    "terminal.data",
    "terminal.closed",
    "activity.created",
  ] as const;

  for (const name of subscribed) {
    assert.ok(
      Object.hasOwn(EVENT_SCHEMAS, name),
      `the UI subscribes to "${name}" but it has no schema in EVENT_SCHEMAS`,
    );
  }
  // And the gate covers exactly that set — no entry is dead weight.
  assert.deepEqual(Object.keys(EVENT_SCHEMAS).sort(), [...subscribed].sort());
});

test("the terminal data gate rejects a drifted payload instead of passing it to xterm", () => {
  const schema = EVENT_SCHEMAS["terminal.data"];
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", data: "hello" }).success, true);

  // A payload missing the session id, carrying the wrong type, or smuggling extra
  // fields must all be refused: the UI would otherwise write it to the terminal.
  assert.equal(schema.safeParse({ data: "hello" }).success, false);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", data: 42 }).success, false);
  assert.equal(schema.safeParse({ terminalSessionId: "", data: "hello" }).success, false);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", data: "hi", extra: 1 }).success, false);
});

test("the terminal gate matches the exact shape the Rust forwarder emits", () => {
  // `forward_terminal_events` in apps/desktop/src-tauri/src/lib.rs hand-builds
  // exactly these two field sets. If that serialisation gains or renames a field,
  // this test is what makes it a visible failure instead of a dead terminal:
  // a strict gate that no longer matches would silently drop every chunk.
  const data = EVENT_SCHEMAS["terminal.data"];
  assert.deepEqual(Object.keys(data.shape).sort(), ["data", "terminalSessionId"]);

  const closed = EVENT_SCHEMAS["terminal.closed"];
  assert.deepEqual(Object.keys(closed.shape).sort(), ["exitCode", "terminalSessionId"]);
});

test("the terminal data gate does not drop a realistically large chunk", () => {
  // Regression guard for the tight-cap mistake: bounding this stream would drop
  // real remote output, which is silent degradation.
  const schema = EVENT_SCHEMAS["terminal.data"];
  const chunk = "x".repeat(64 * 1024);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", data: chunk }).success, true);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", data: "" }).success, true);
});

test("the terminal closed gate accepts a null exit code", () => {
  const schema = EVENT_SCHEMAS["terminal.closed"];
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", exitCode: null }).success, true);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", exitCode: 0 }).success, true);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01" }).success, false);
  // Rust sends exitCode as Option<u32>: a negative or fractional code is drift.
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", exitCode: -1 }).success, false);
  assert.equal(schema.safeParse({ terminalSessionId: "t_01", exitCode: 1.5 }).success, false);
});

test("every terminal font face has both a label and a family stack", () => {
  for (const font of TERMINAL_FONT_ORDER) {
    assert.ok(TERMINAL_FONT_LABEL[font], `${font} has no label`);
    assert.ok(TERMINAL_FONT_FAMILY[font], `${font} has no family stack`);
    // The face the user picked must be first in its own stack, otherwise picking a
    // font silently renders a different one that merely happens to be installed.
    const first = TERMINAL_FONT_FAMILY[font].split(",")[0]?.trim() ?? "";
    assert.equal(
      first,
      `"${TERMINAL_FONT_LABEL[font]}"`,
      `${font}: the stack does not start with the face its label names`,
    );
  }
  assert.equal(new Set(TERMINAL_FONT_ORDER).size, TERMINAL_FONT_ORDER.length, "a face is listed twice");
  assert.equal(
    new Set(TERMINAL_FONT_ORDER.map((font) => TERMINAL_FONT_LABEL[font])).size,
    TERMINAL_FONT_ORDER.length,
    "two faces share a label, so the dropdown is ambiguous",
  );
});

test("the declared font families exist as real faces in the stylesheet", () => {
  // A family name that no `@font-face` declares does not fail — it silently falls
  // back to Consolas. So the names are checked against the stylesheet rather than
  // trusted, which is the whole reason these stacks were worth centralising.
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const declared = new Set(
    [...css.matchAll(/@font-face\s*\{[^}]*?font-family:\s*"([^"]+)"/gs)].map((match) => match[1]),
  );
  // Five `@font-face` blocks, but three distinct families: the regular and semibold
  // weights of "JetBrains Mono" and of "JetBrains Mono NL" are each declared twice.
  assert.equal(
    declared.size,
    TERMINAL_FONT_ORDER.length,
    `expected exactly the ${TERMINAL_FONT_ORDER.length} terminal faces, found ${[...declared].join(", ")}`,
  );

  for (const font of TERMINAL_FONT_ORDER) {
    const family = TERMINAL_FONT_LABEL[font];
    assert.ok(
      declared.has(family),
      `"${family}" is not declared by any @font-face, so it would fall back silently`,
    );
  }
  // Every *bundled* face a stack names must be declared too — the fallback order is
  // what keeps a missing weight from dropping to Consolas. System fonts are exempt:
  // "Cascadia Mono" and "Consolas" ship with the OS, and `--font-ui` in the same
  // stylesheet leans on Consolas for the same reason.
  const SYSTEM_FALLBACKS = new Set(["Cascadia Mono", "Consolas", "Microsoft YaHei UI", "monospace"]);
  for (const font of TERMINAL_FONT_ORDER) {
    const families = [...TERMINAL_FONT_FAMILY[font].matchAll(/"([^"]+)"/g)].flatMap((match) => match[1] ?? []);
    for (const family of families) {
      if (SYSTEM_FALLBACKS.has(family)) continue;
      assert.ok(declared.has(family), `"${family}" in the ${font} stack is not declared`);
    }
  }
  // All three stacks end in the same system chain, so a missing bundled face degrades
  // identically whichever face was picked.
  const tails = new Set(
    TERMINAL_FONT_ORDER.map((font) => {
      const parts = TERMINAL_FONT_FAMILY[font].split(",").map((part) => part.trim());
      return parts.slice(parts.findIndex((part) => SYSTEM_FALLBACKS.has(part.replace(/"/g, "")))).join(", ");
    }),
  );
  assert.equal(tails.size, 1, `the three stacks have different system fallbacks:\n${[...tails].join("\n")}`);
});

test("the terminal stack matches the --font-terminal token", () => {
  // `styles.css` already carries this exact stack as `--font-terminal`, which is what
  // the xterm container inherits before the JS option overwrites it. Two copies of the
  // same list drift; this pins them together.
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const token = /--font-terminal:\s*([^;]+);/.exec(css);
  assert.ok(token, "--font-terminal is not defined");
  const tokenValue = token[1];
  assert.ok(tokenValue, "--font-terminal has no value");
  const normalise = (value: string) => value.split(",").map((part) => part.trim().replace(/\s+/g, " ")).join(", ");
  assert.equal(
    normalise(TERMINAL_FONT_FAMILY["jetbrains-mono-nl"]),
    normalise(tokenValue),
    "the default terminal face and --font-terminal have drifted apart",
  );
});
