/**
 * 工作页种类到图标名的唯一映射。
 *
 * 标签栏（`WorkspaceTabStrip`）与内容框（`WorkpagesDock`）都要画同一个图标；两处各写
 * 一份的话，有一天只有一处被改，标签和页头就会显示不同的图形，而没有任何东西会报错。
 * 这里刻意只是一个查表，不含渲染与状态。
 */

import type { IconName } from "../../components/Icon.js";
import type { Workpage } from "./workpage-model.js";

export const PAGE_ICON: Record<Workpage["kind"], IconName> = {
  board: "board",
  files: "folder",
  terminal: "terminal",
  logs: "logs",
  services: "services",
  evidence: "activity",
  artifact: "file",
};
