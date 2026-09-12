import { AddServerInputSchema, UpdateServerInputSchema, type AddServerInput, type UpdateServerInput, type Environment } from "@yukinal/shared";

export interface ServerFormValues {
  name: string;
  host: string;
  port: string;
  username: string;
  environment: Environment;
  /** `agent` 不需要任何输入：它是对「用本机 ssh-agent 里的身份认证」的一次选择。 */
  authMethod: "password" | "privateKey" | "agent";
  password: string;
  privateKeyPem: string;
  /** 加密私钥的口令。可选 —— 明文 key 留空即可。 */
  passphrase: string;
}

export const SERVER_FORM_VALIDATION_MESSAGES = {
  invalidPort: "端口必须是 1 到 65535 之间的整数。",
  missingConnectionFields: "请填写名称、主机和用户名。",
  missingPassword: "请填写 SSH 密码。",
  missingPrivateKey: "请填写 SSH 私钥。",
} as const;

/**
 * 口令的提交规则，与 Rust 侧「空 / 纯空白 = 没有口令」对齐。
 *
 * 留空时返回 `undefined`，调用点据此**省略字段**：契约里 `passphrase` 是
 * `min(1)` 的可选字段，送一个 `""` 会被 schema 拒掉，而「明文 key」本身就是
 * 不带这个字段的形状。前后空格只在判断时忽略，材料原样送出 —— 口令里的空格
 * 是口令的一部分，擅自 trim 会把一把能用的 key 变成解不开的 key。
 */
function passphraseForSubmission(passphrase: string): string | undefined {
  return passphrase.trim() ? passphrase : undefined;
}

export function buildServerInput(values: ServerFormValues): AddServerInput;
export function buildServerInput(values: ServerFormValues, serverId: string): UpdateServerInput;
export function buildServerInput(values: ServerFormValues, serverId?: string): AddServerInput | UpdateServerInput {
  const port = Number(values.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error(SERVER_FORM_VALIDATION_MESSAGES.invalidPort);
  const base = { name: values.name.trim(), host: values.host.trim(), port, username: values.username.trim(), environment: values.environment };
  if (!base.name || !base.host || !base.username) throw new Error(SERVER_FORM_VALIDATION_MESSAGES.missingConnectionFields);

  // ssh-agent 不带任何 secret，所以「没输入 secret 就省略 authentication」这条规则
  // 对它不适用：**选它本身就是指令**。编辑时省略 authentication 会被后端读成
  // 「保留现有凭据」，那等于用户选了 agent、实际还在用旧密码连。
  if (values.authMethod === "agent") {
    const authentication = { method: "agent" as const };
    return serverId
      ? UpdateServerInputSchema.parse({ ...base, serverId, authentication })
      : AddServerInputSchema.parse({ ...base, authentication });
  }

  // 密码 / 私钥：只有真的输入了 secret 才替换现有凭据。
  // 留空 = 不碰已存的认证（编辑的关键语义），首次添加则由下面的校验报错。
  const secret = values.authMethod === "password" ? values.password : values.privateKeyPem.trim();
  const submittedPassphrase = passphraseForSubmission(values.passphrase);
  const authentication = values.authMethod === "password"
    ? { method: "password" as const, password: secret }
    : {
        method: "privateKey" as const,
        privateKeyPem: secret,
        ...(submittedPassphrase === undefined ? {} : { passphrase: submittedPassphrase }),
      };
  if (serverId) return UpdateServerInputSchema.parse({ ...base, serverId, ...(secret ? { authentication } : {}) });
  if (!secret) throw new Error(values.authMethod === "password" ? SERVER_FORM_VALIDATION_MESSAGES.missingPassword : SERVER_FORM_VALIDATION_MESSAGES.missingPrivateKey);
  return AddServerInputSchema.parse({ ...base, authentication });
}
