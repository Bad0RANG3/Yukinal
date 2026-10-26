/**
 * Heading anchors, shared by the docs link gate and its tests.
 *
 * This exists because of a bug that was invisible for weeks: the gate split the
 * file on `"\n"` and anchored with `/^#{1,6}\s+/`. Three documents had been written
 * with CRLF endings, so every heading line actually ended in `\r`. The heading
 * regex still matched (`\r` is whitespace, so `\s+` swallowed what preceded it and
 * `(.*)` did not include the trailing `\r`), but the *anchor set* was built from a
 * line the gate had already failed to recognise in some editors' output — and the
 * real symptom was worse than that: one file's anchors came out **empty**, so a
 * link into it reported "no heading for anchor" even though the heading was right
 * there.
 *
 * Two things are therefore deliberate here:
 *
 *   1. `\r\n` is normalised to `\n` before anything else. Line endings are
 *      invisible, so a gate that depends on them is a gate that fails for reasons
 *      the author cannot see.
 *   2. The anchor function is exported. The gate is the only production caller, but
 *      the behaviour is pinned by tests now, which is what stops the next rewrite
 *      from re-introducing a slice-the-array trick that works on one platform.
 */

/** GitHub's heading normalisation: lowercase, drop punctuation, spaces to dashes. */
export function anchorize(heading) {
  return String(heading)
    .trim()
    .toLowerCase()
    .replace(/[`*_~]/g, "")
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .trim()
    .replace(/\s+/g, "-");
}

/** Normalise a document's line endings so callers never have to think about them. */
export function normalizeNewlines(source) {
  return String(source).replace(/\r\n?/g, "\n");
}

/**
 * Anchors a document declares, in document order, ignoring fenced code blocks.
 *
 * A fenced block can contain `#` comment lines; treating those as headings would
 * manufacture anchors that no renderer produces.
 */
export function headingAnchors(source) {
  const anchors = [];
  let fenced = false;
  for (const line of normalizeNewlines(source).split("\n")) {
    if (/^\s*```/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const heading = /^#{1,6}\s+(.*)$/.exec(line);
    if (heading) anchors.push(anchorize(heading[1]));
  }
  return anchors;
}
