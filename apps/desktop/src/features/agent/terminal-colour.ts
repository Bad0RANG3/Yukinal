/**
 * Terminal SGR colour for tool output the user opens on purpose.
 *
 * Why this is not an HTML/text escape problem
 * -------------------------------------------
 * Every answer and every tool card in the conversation is rendered as **text**:
 * `<pre>{stdout}</pre>` and `<KeywordText text={...}>`. React escapes it, so an
 * ANSI sequence that reaches the transcript is shown literally as `\x1b[32m`,
 * which is worse than nothing. This module therefore returns a data structure,
 * not markup, and the renderer builds `<span>` elements from it. There is no
 * `dangerouslySetInnerHTML` and no place for output to become markup.
 *
 * The threat that matters here is different: the bytes come from a server we do
 * not control, and a terminal is a program. ESC can move the cursor, change the
 * scroll region, query the device, change the window title, or write the
 * clipboard. So this is an **allowlist of SGR parameters only**:
 *
 *  - `ESC [ ... m` with parameters drawn from a fixed set (0-9, 22-29, 30-49,
 *    90-107, plus 38/48/58 extended forms);
 *  - every other C0 control character is removed, ESC included;
 *  - a sequence that does not match exactly is dropped, never guessed at.
 *
 * Colour is applied in place, so `ls --color`, `systemctl status` and
 * `journalctl` output keep their meaning instead of becoming three lines of
 * escaped noise.
 */

export type StyledRun = {
  /** The literal text. Never contains a control character. */
  text: string;
  /** Intent resolved from SGR, so the renderer needs no colour table of its own. */
  tone: "plain" | "error" | "warning" | "success" | "accent" | "muted" | "bold";
};

const ESC = "\u001b";

/** SGR parameters we accept. Anything else drops the whole sequence. */
const ALLOWED_SGR = /^(?:[0-9]{1,3}(?:;[0-9]{1,3})*)?$/;

const BOLD = new Set(["1"]);
const DIM = new Set(["2"]);
const RED = new Set(["31", "91", "38;5;1", "38;5;9", "38;5;160", "38;5;196"]);
const YELLOW = new Set(["33", "93", "38;5;3", "38;5;11", "38;5;220"]);
const GREEN = new Set(["32", "92", "38;5;2", "38;5;10", "38;5;34", "38;5;46"]);
const CYAN = new Set(["36", "96", "38;5;6", "38;5;14", "38;5;39", "38;5;51"]);

/** Strip every C0 control character except a tab and a newline. */
function stripControls(value: string): string {
  // eslint-disable-next-line no-control-regex
  return value.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, "");
}

/**
 * Parse one tool output string into styled runs.
 *
 * Unrecognised escapes are removed rather than shown, because a raw `\x1b[?25l`
 * in a `<pre>` is both useless and alarming to the operator.
 */
export function parseTerminalRuns(input: string): StyledRun[] {
  const runs: StyledRun[] = [];
  let tone: StyledRun["tone"] = "plain";
  let buffer = "";

  const flush = () => {
    if (!buffer) return;
    runs.push({ text: stripControls(buffer), tone });
    buffer = "";
  };

  for (let index = 0; index < input.length; index += 1) {
    const character = input[index];
    if (character !== ESC) {
      buffer += character;
      continue;
    }
    const next = input[index + 1];
    if (next === "]") {
      // Operating System Command: consume the whole string up to BEL or ST, then
      // drop it. A window-title or clipboard payload must not become visible text.
      let cursor = index + 2;
      while (cursor < input.length) {
        if (input[cursor] === "\u0007") { cursor += 1; break; }
        if (input[cursor] === ESC && input[cursor + 1] === "\\") { cursor += 2; break; }
        cursor += 1;
      }
      index = cursor - 1;
      continue;
    }
    if (next !== "[") {
      // Other two-or-three byte escapes: `ESC ( B` and friends consume one
      // following byte; anything else consumes only the introducer.
      index += "()*+#%&".includes(next ?? "") ? 2 : 1;
      continue;
    }
    // CSI: parameters, then a final byte in 0x40-0x7E.
    let cursor = index + 2;
    while (cursor < input.length) {
      const code = input.charCodeAt(cursor);
      if (code >= 0x40 && code <= 0x7e) break;
      if (code < 0x20 || code > 0x3f) break;
      cursor += 1;
    }
    const finalByte = input[cursor];
    const parameters = input.slice(index + 2, cursor);
    index = cursor;
    if (finalByte !== "m" || !ALLOWED_SGR.test(parameters)) {
      // Cursor moves, title changes, device queries and private modes are all
      // dropped. They are the ones that can alter a terminal's state.
      continue;
    }
    flush();
    tone = toneForSequence(parameters, tone);
  }
  flush();
  return runs;
}

function toneForSequence(parameters: string, current: StyledRun["tone"]): StyledRun["tone"] {
  const codes: string[] = parameters === "" ? ["0"] : parameters.split(";");
  let tone = current;
  // Codes apply left to right, so the last colour wins; 0 resets everything.
  for (let index = 0; index < codes.length; index += 1) {
    const code = codes[index] ?? "";
    if (code === "0") {
      tone = "plain";
      continue;
    }
    // `38;5;N` is three tokens; consume the rest of the extended form as one unit.
    if (code === "38" || code === "48" || code === "58") {
      const rest = codes.slice(index).join(";");
      const end = code === "38" ? rest.match(/^(38;(?:5;[0-9]{1,3}|2;[0-9]{1,3};[0-9]{1,3};[0-9]{1,3}))/) : null;
      const matched = end === null ? undefined : end[1];
      if (matched !== undefined) {
        tone = toneForColorNumber(matched, tone);
        index += matched.split(";").length - 1;
      } else {
        index = codes.length;
      }
      continue;
    }
    tone = toneForColorNumber(code, tone);
  }
  return tone;
}

function toneForColorNumber(code: string, current: StyledRun["tone"]): StyledRun["tone"] {
  if (BOLD.has(code)) return "bold";
  if (DIM.has(code)) return "muted";
  if (RED.has(code)) return "error";
  if (YELLOW.has(code)) return "warning";
  if (GREEN.has(code)) return "success";
  if (CYAN.has(code)) return "accent";
  // 39 is the default foreground; any other colour is not worth a tone, but the
  // text is still kept — the operator sees content, not a colour we invented.
  if (code === "39") return "plain";
  return current;
}

/** True when the string carries any escape at all; used to skip the parser. */
export function hasTerminalEscapes(value: string): boolean {
  return value.includes(ESC);
}
