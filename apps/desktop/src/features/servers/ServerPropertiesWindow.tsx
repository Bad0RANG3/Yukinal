import { useState } from "react";
import type { Server } from "@yukinal/shared";
import { SecondaryWindow, PropertyList, PropertySection } from "../../components/SecondaryWindow.js";
import { ENVIRONMENT_LABEL_SHORT, SERVER_STATUS_LABEL } from "../../lib/labels.js";
import { formatTimestamp } from "../../lib/format.js";

const TABS = [{ id: "overview", label: "概览" }, { id: "capabilities", label: "环境与能力" }];
const CAPABILITIES: Record<keyof Server["capabilities"], string> = { linux: "Linux", docker: "Docker", systemd: "systemd", nginx: "Nginx", postgres: "PostgreSQL", redis: "Redis", kubernetes: "Kubernetes" };
export function ServerPropertiesWindow({ server, onClose }: { server: Server; onClose: () => void }) {
  const [tab, setTab] = useState("overview");
  return <SecondaryWindow title="服务器属性" subtitle={server.name} icon="servers" tabs={TABS} activeTab={tab} onTabChange={setTab} onClose={onClose}
    footer={<span>更新于 {formatTimestamp(server.updatedAt)}</span>}>
    {tab === "overview" ? <>
      <PropertySection title="服务器"><PropertyList items={[{ label: "名称", value: server.name }, { label: "连接状态", value: SERVER_STATUS_LABEL[server.status] }, { label: "环境", value: ENVIRONMENT_LABEL_SHORT[server.metadata.environment] }, { label: "服务器 ID", value: server.id, mono: true }]} /></PropertySection>
      <PropertySection title="连接"><PropertyList items={[{ label: "主机", value: server.connection.host, mono: true }, { label: "端口", value: server.connection.port }, { label: "用户", value: server.connection.username }, { label: "认证引用", value: server.connection.identityId ? "已配置" : "未配置" }]} /></PropertySection>
    </> : <>
      <PropertySection title="环境信息"><PropertyList items={[{ label: "系统", value: server.metadata.os ?? "未记录" }, { label: "主机名", value: server.metadata.hostname ?? "未记录" }, { label: "区域", value: server.metadata.region ?? "未记录" }, { label: "标签", value: server.metadata.tags?.join("、") || "未记录" }]} /></PropertySection>
      <PropertySection title="已记录能力"><PropertyList items={(Object.entries(CAPABILITIES) as [keyof Server["capabilities"], string][]).map(([key, label]) => ({ label, value: server.capabilities[key] === true ? "已发现" : server.capabilities[key] === false ? "未发现" : "未检测" }))} /></PropertySection>
    </>}
  </SecondaryWindow>;
}
