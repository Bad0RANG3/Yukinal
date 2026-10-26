/**
 * Regression guards for the documentation anchor helper.
 *
 * The gate had a bug that no test could see: it split documents on `"\n"` and
 * anchored every heading line. A document written with CRLF endings then produced
 * *no* anchors at all (or, depending on where the `\r` landed, an empty-string
 * anchor), so a link into a heading that was visibly present failed with "no
 * heading for anchor". The gate itself was green-but-wrong in both directions:
 * it did not catch the real breakage, and the "fix" of normalising the three
 * affected files by hand would have rotted again on the next Windows edit.
 *
 * These tests pin the *helper* rather than the gate's output, because the helper is
 * where the normalisation now lives and where the next rewrite will happen.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { anchorize, headingAnchors, normalizeNewlines } from "./doc-anchors.mjs";

test("CRLF and LF documents declare exactly the same anchors", () => {
  const markdown = "# Title\n\n## 用户批准意味着什么\n\n### Sub section\n";
  const crlf = markdown.replace(/\n/g, "\r\n");
  assert.deepEqual(headingAnchors(crlf), headingAnchors(markdown));
  // The failure mode that motivated the helper: before normalisation the CRLF
  // document anchored as the empty string, which silently "matched" nothing.
  assert.deepEqual(headingAnchors(crlf), ["title", "用户批准意味着什么", "sub-section"]);
  assert.equal(headingAnchors(crlf).includes(""), false);
});

test("a lone CR is normalised too, so old-Mac endings cannot anchor as empty", () => {
  assert.equal(normalizeNewlines("a\rb\r\nc"), "a\nb\nc");
  assert.deepEqual(headingAnchors("# A\r## B\r"), ["a", "b"]);
});

test("headings inside a fenced block are not anchors", () => {
  const markdown = "# Real\n\n```bash\n# not a heading\n```\n";
  assert.deepEqual(headingAnchors(markdown), ["real"]);
});

test("anchorize drops punctuation and markdown emphasis the way GitHub does", () => {
  assert.equal(anchorize("## `code` and **bold**"), "code-and-bold");
  assert.equal(anchorize("  Mixed CASE  "), "mixed-case");
  assert.equal(anchorize("What's new?"), "whats-new");
  // Non-Latin scripts survive: the documentation is bilingual.
  assert.equal(anchorize("从哪里开始"), "从哪里开始");
});

test("an empty heading is not manufactured into an anchor", () => {
  // `#` alone is not a heading in GitHub's renderer. The helper treats it as one
  // line that does not match, so no empty-string anchor can ever be satisfied.
  assert.deepEqual(headingAnchors("#\n##\n"), []);
});
