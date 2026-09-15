/**
 * The icon contract, pinned.
 *
 * Icon alignment is the kind of defect that never shows up in review: an SVG
 * that is half a pixel off, or a scale that quietly grows a seventh step, looks
 * fine in a diff and wrong on screen. These tests exist so the contract has to
 * be changed on purpose rather than by accident.
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import { Icon, ICON_SIZE, type IconName } from "../src/components/Icon.js";

const render = (name: IconName, size?: number | keyof typeof ICON_SIZE) =>
  renderToStaticMarkup(<Icon name={name} {...(size === undefined ? {} : { size })} />);

const attr = (markup: string, name: string): string | null =>
  new RegExp(`${name}="([^"]*)"`).exec(markup)?.[1] ?? null;

test("the size scale is the documented one, in ascending order", () => {
  assert.deepEqual(Object.keys(ICON_SIZE), ["xs", "sm", "md", "lg", "xl", "xxl"]);
  const values = Object.values(ICON_SIZE);
  assert.deepEqual([...values].sort((a, b) => a - b), values, "ICON_SIZE must stay ascending");
  for (const value of values) {
    assert.equal(Number.isInteger(value), true, `${value} must be a whole number of pixels`);
    assert.ok(value >= 10 && value <= 32, `${value} is outside a sensible icon range`);
  }
});

test("every icon renders on one shared viewBox", () => {
  const names: IconName[] = [
    "activity", "agent", "archive", "arrowUp", "chevronDown", "chevronRight", "chevronUp", "close",
    "connect", "copy", "disconnect", "edit", "externalLink", "file", "folder", "logs", "plus", "projects",
    "refresh", "search", "servers", "services", "settings", "shield", "sparkle", "stop",
    "terminal", "trash", "warning",
  ];
  for (const name of names) {
    const markup = render(name);
    assert.equal(attr(markup, "viewBox"), "0 0 24 24", `${name} must use the 24×24 grid`);
    assert.ok(markup.includes("<svg"), `${name} must render an svg`);
  }
});

test("a size step renders as a square of exactly that many pixels", () => {
  for (const [step, px] of Object.entries(ICON_SIZE)) {
    const markup = render("plus", step as keyof typeof ICON_SIZE);
    assert.equal(attr(markup, "width"), String(px), `${step} width`);
    assert.equal(attr(markup, "height"), String(px), `${step} height`);
  }
});

test("an explicit pixel size still works for one-off cases", () => {
  const markup = render("plus", 20);
  assert.equal(attr(markup, "width"), "20");
  assert.equal(attr(markup, "height"), "20");
});

/**
 * The reason stroke width is computed rather than fixed: a constant value in
 * viewBox units renders thinner as the icon shrinks, so a 12px chevron and a
 * 26px empty-state mark stop looking like the same drawing. Solving for a
 * constant on-screen stroke is what keeps the set optically consistent.
 */
test("the stroke is optically constant across the whole scale", () => {
  const optical = Object.entries(ICON_SIZE).map(([step, px]) => {
    const stroke = Number(attr(render("plus", step as keyof typeof ICON_SIZE), "stroke-width"));
    assert.ok(Number.isFinite(stroke), `${step} must declare a numeric stroke width`);
    return (stroke * px) / 24;
  });
  const min = Math.min(...optical);
  const max = Math.max(...optical);
  assert.ok(max - min < 0.35, `rendered stroke drifts by ${(max - min).toFixed(2)}px: ${optical.map((n) => n.toFixed(2)).join(", ")}`);
  for (const px of optical) assert.ok(px >= 1.4 && px <= 1.7, `stroke ${px.toFixed(2)}px is outside the intended weight`);
});

test("an explicit stroke width overrides the correction", () => {
  const markup = renderToStaticMarkup(<Icon name="plus" size="md" strokeWidth={3} />);
  assert.equal(attr(markup, "stroke-width"), "3");
});

/**
 * Every icon is decorative: the visible label always carries the meaning, and
 * the control around it carries the accessible name. An icon that announces
 * itself would double every label a screen reader reads out.
 */
test("icons are hidden from assistive technology", () => {
  assert.equal(attr(render("plus"), "aria-hidden"), "true");
  assert.equal(attr(render("plus"), "focusable"), "false");
});

test("the icon class is present so the layout contract in styles.css applies", () => {
  assert.equal(attr(render("plus"), "class"), "icon");
});
