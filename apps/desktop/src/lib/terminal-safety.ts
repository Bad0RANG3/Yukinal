/**
 * Stateful terminal boundary. Only validated SGR may reach xterm; OSC/DCS/APC,
 * device queries, cursor movement and modes are consumed, including across IPC
 * chunks. Never reset this parser between chunks of one remote session.
 */
export class TerminalOutputFilter {
  private state: "text" | "escape" | "intermediate" | "csi" | "string" | "stringEscape" = "text";
  private parameters = "";
  private oversized = false;
  private osc = false;

  constructor(private readonly colors = true, private readonly carriageReturn = true) {}

  write(chunk: string): string {
    let output = "";
    for (const character of chunk) {
      const code = character.codePointAt(0)!;
      if (this.state === "string" || this.state === "stringEscape") {
        if (code === 0x9c || (this.osc && code === 7)
          || (this.state === "stringEscape" && character === "\\")) {
          this.state = "text";
        } else {
          this.state = code === 0x1b ? "stringEscape" : "string";
        }
        continue;
      }
      if (code === 0x1b) {
        this.state = "escape";
        continue;
      }
      if (code === 0x9b) {
        this.startCsi();
        continue;
      }
      if ([0x90, 0x98, 0x9d, 0x9e, 0x9f].includes(code)) {
        this.osc = code === 0x9d;
        this.state = "string";
        continue;
      }
      if (this.state === "escape") {
        if (character === "[") this.startCsi();
        else if ("]PX^_".includes(character)) {
          this.osc = character === "]";
          this.state = "string";
        } else this.state = code >= 0x20 && code <= 0x2f ? "intermediate" : "text";
        continue;
      }
      if (this.state === "intermediate") {
        if (code < 0x20 || code > 0x2f) this.state = "text";
        continue;
      }
      if (this.state === "csi") {
        if (code >= 0x40 && code <= 0x7e) {
          if (character === "m" && !this.oversized && this.colors && validSgr(this.parameters)) {
            output += `\x1b[${this.parameters}m`;
          }
          this.state = "text";
        } else if (code >= 0x20 && code <= 0x3f) {
          if (this.parameters.length < 128) this.parameters += character;
          else this.oversized = true;
        } else {
          // Fail closed on malformed controls until the final CSI byte arrives.
          this.oversized = true;
        }
        continue;
      }
      if (character === "\t" || character === "\n" || (this.carriageReturn && character === "\r")) {
        output += character;
      } else if (code >= 0x20 && !(code >= 0x7f && code <= 0x9f)
        && !(code >= 0x202a && code <= 0x202e) && !(code >= 0x2066 && code <= 0x2069)) {
        output += character;
      }
    }
    return output;
  }

  private startCsi(): void {
    this.state = "csi";
    this.parameters = "";
    this.oversized = false;
  }
}

/** Keep only display attributes and bounded 8/24-bit colour forms. */
function validSgr(parameters: string): boolean {
  if (parameters === "") return true;
  if (!/^[0-9]{1,3}(?:;[0-9]{1,3})*$/.test(parameters)) return false;
  const values = parameters.split(";").map(Number);
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index]!;
    if (value === 38 || value === 48) {
      const mode = values[++index];
      const count = mode === 5 ? 1 : mode === 2 ? 3 : 0;
      if (!count) return false;
      for (let component = 0; component < count; component += 1) {
        const channel = values[++index];
        if (channel === undefined || channel > 255) return false;
      }
    } else if (!(value <= 4 || value === 7 || value === 9 || value === 22 || value === 23
      || value === 24 || value === 27 || value === 29 || (value >= 30 && value <= 37)
      || value === 39 || (value >= 40 && value <= 47) || value === 49
      || (value >= 90 && value <= 97) || (value >= 100 && value <= 107))) return false;
  }
  return true;
}

/** Markdown and copyable text never receive ANSI, including SGR. */
export function stripTerminalControls(value: string): string {
  return new TerminalOutputFilter(false, false).write(value);
}
