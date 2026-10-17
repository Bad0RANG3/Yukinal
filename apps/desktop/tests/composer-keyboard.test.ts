import assert from "node:assert/strict";
import test from "node:test";
import { composerKeyAction } from "../src/features/agent/composer-keyboard.js";

const closed = { open: false, canComplete: false };
const suggestions = { open: true, canComplete: true };

test("Enter remains a newline while drafting, including when sending is unavailable", () => {
  assert.equal(composerKeyAction({ key: "Enter" }, closed), "native");
  assert.equal(composerKeyAction({ key: "Enter", shiftKey: true }, suggestions), "native");
  assert.equal(composerKeyAction({ key: "Enter" }, { open: true, canComplete: false }), "native");
});

test("send shortcut bypasses completion while plain Enter and Tab accept a suggestion", () => {
  assert.equal(composerKeyAction({ key: "Enter", ctrlKey: true }, suggestions), "submit");
  assert.equal(composerKeyAction({ key: "Enter", metaKey: true }, closed), "submit");
  assert.equal(composerKeyAction({ key: "Enter", ctrlKey: true, shiftKey: true }, closed), "native");
  assert.equal(composerKeyAction({ key: "Enter" }, suggestions), "complete");
  assert.equal(composerKeyAction({ key: "Tab" }, suggestions), "complete");
  assert.equal(composerKeyAction({ key: "Tab", shiftKey: true }, suggestions), "native");
});

test("IME confirmation cannot complete a mention, submit, or move suggestion selection", () => {
  for (const key of ["Enter", "ArrowDown", "Escape"]) {
    assert.equal(composerKeyAction({ key, isComposing: true }, suggestions), "native");
    assert.equal(composerKeyAction({ key, keyCode: 229, ctrlKey: true }, suggestions), "native");
  }
  assert.equal(composerKeyAction({ key: "ArrowDown" }, suggestions), "next");
  assert.equal(composerKeyAction({ key: "ArrowUp" }, suggestions), "previous");
  assert.equal(composerKeyAction({ key: "Escape" }, suggestions), "dismiss");
});
