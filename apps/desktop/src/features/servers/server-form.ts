import { AddServerInputSchema, UpdateServerInputSchema, type AddServerInput, type UpdateServerInput, type Environment } from "@yukinal/shared";

export interface ServerFormValues {
  name: string;
  host: string;
  port: string;
  username: string;
  environment: Environment;
  authMethod: "password" | "privateKey";
  password: string;
  privateKeyPem: string;
}

export function buildServerInput(values: ServerFormValues): AddServerInput;
export function buildServerInput(values: ServerFormValues, serverId: string): UpdateServerInput;
export function buildServerInput(values: ServerFormValues, serverId?: string): AddServerInput | UpdateServerInput {
  const port = Number(values.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("端口必须是 1–65535 之间的整数。");
  const base = { name: values.name.trim(), host: values.host.trim(), port, username: values.username.trim(), environment: values.environment };
  if (!base.name || !base.host || !base.username) throw new Error("请填写名称、主机和用户名。");
  // Only the selected authentication method can replace the existing secret.
  const secret = values.authMethod === "password" ? values.password : values.privateKeyPem.trim();
  const authentication = values.authMethod === "password"
    ? { method: "password" as const, password: secret }
    : { method: "privateKey" as const, privateKeyPem: secret };
  if (serverId) return UpdateServerInputSchema.parse({ ...base, serverId, ...(secret ? { authentication } : {}) });
  if (!secret) throw new Error(values.authMethod === "password" ? "请填写 SSH 密码。" : "请填写 SSH 私钥。");
  return AddServerInputSchema.parse({ ...base, authentication });
}
