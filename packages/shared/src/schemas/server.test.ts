import assert from "node:assert/strict";
import test from "node:test";

import { AddServerInputSchema, ServerSchema, UpdateServerInputSchema } from "./server.js";
import { PermissionDecisionSchema } from "./permission.js";

const validServer = {
  id: "srv_01abc",
  name: "Production API",
  connection: { host: "api.example.com", port: 22, username: "deploy" },
  capabilities: { linux: true, docker: true },
  status: "connected",
  metadata: { environment: "production", region: "Singapore" },
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
};

test("accepts a contract-shaped server", () => {
  assert.equal(ServerSchema.safeParse(validServer).success, true);
});

test("rejects host-derived ids and non-ports", () => {
  assert.equal(
    ServerSchema.safeParse({ ...validServer, id: "api.example.com:22" }).success,
    false,
  );
  assert.equal(
    ServerSchema.safeParse({ ...validServer, connection: { ...validServer.connection, port: 0 } })
      .success,
    false,
  );
});

test("unknown environments must be declared, never guessed", () => {
  assert.equal(
    AddServerInputSchema.safeParse({
      name: "db",
      host: "10.0.0.5",
      username: "root",
      environment: "prod-ish",
      authentication: { method: "identity", identityId: "idn_1" },
    }).success,
    false,
  );
});

test("an add-server payload can carry a secret without the schema leaking it into types", () => {
  const parsed = AddServerInputSchema.safeParse({
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: { method: "password", password: "hunter2" },
  });
  assert.equal(parsed.success, true);
});

const agentAdd = {
  name: "db",
  host: "10.0.0.5",
  username: "root",
  environment: "staging",
  authentication: { method: "agent" },
};

test("the ssh-agent option carries no secret, and nothing else may ride along", () => {
  assert.equal(AddServerInputSchema.safeParse(agentAdd).success, true);
  assert.equal(
    UpdateServerInputSchema.safeParse({ ...agentAdd, serverId: "srv_01abc" }).success,
    true,
  );
  // `.strictObject`：agent 是「一个 secret 都不带」的形状，不是「可以顺便带一个」。
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      authentication: { method: "agent", password: "smuggled" },
    }).success,
    false,
  );
});

test("an encrypted private key carries an optional passphrase with the same bounds as Rust", () => {
  const withPassphrase = {
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: {
      method: "privateKey",
      privateKeyPem: "-----BEGIN OPENSSH PRIVATE KEY-----",
      passphrase: "hunter2",
    },
  };
  assert.equal(AddServerInputSchema.safeParse(withPassphrase).success, true);
  // 省略 passphrase = 明文 key：这是合法形状，不是「填漏了」。
  assert.equal(
    AddServerInputSchema.safeParse({
      ...withPassphrase,
      authentication: { method: "privateKey", privateKeyPem: "-----BEGIN OPENSSH PRIVATE KEY-----" },
    }).success,
    true,
  );
  // 空串不是「空口令」：没填就该省略字段（`min(1)` 与 Rust 侧一致）。
  assert.equal(
    AddServerInputSchema.safeParse({
      ...withPassphrase,
      authentication: { ...withPassphrase.authentication, passphrase: "" },
    }).success,
    false,
  );
  // 与 Rust 的 `max(4_096)` 对齐：4_096 收，4_097 拒。
  assert.equal(
    AddServerInputSchema.safeParse({
      ...withPassphrase,
      authentication: { ...withPassphrase.authentication, passphrase: "p".repeat(4_096) },
    }).success,
    true,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...withPassphrase,
      authentication: { ...withPassphrase.authentication, passphrase: "p".repeat(4_097) },
    }).success,
    false,
  );
});

test("certificate authentication requires a certificate path and keeps the key transient", () => {
  const certificate = {
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: {
      method: "certificate",
      privateKeyPem: "-----BEGIN OPENSSH PRIVATE KEY-----",
      certificatePath: "/home/dev/.ssh/id_ed25519-cert.pub",
      privateKeyPath: "/home/dev/.ssh/id_ed25519",
    },
  };
  const parsed = AddServerInputSchema.safeParse(certificate);
  assert.equal(parsed.success, true);
  if (parsed.success) {
    const authentication = parsed.data.authentication;
    assert.equal(authentication.method, "certificate");
    if (authentication.method === "certificate") {
      assert.equal(authentication.certificatePath, "/home/dev/.ssh/id_ed25519-cert.pub");
      assert.equal("credentialRef" in authentication, false);
    }
  }
  assert.equal(
    AddServerInputSchema.safeParse({
      ...certificate,
      authentication: {
        method: "certificate",
        privateKeyPem: "-----BEGIN OPENSSH PRIVATE KEY-----",
      },
    }).success,
    false,
    "a certificate identity needs an explicit public certificate path",
  );
});

test("host certificate authority is explicit and clearable without accepting both states", () => {
  const authority = {
    caPublicKey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest host-ca",
    principals: ["*.example.test", "api.internal"],
    revocationListPath: "/etc/ssh/revoked_hosts.krl",
    revocationListSigners: [
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKrlOld krl-old",
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKrlNew krl-new",
    ],
  };
  assert.equal(AddServerInputSchema.safeParse({ ...agentAdd, hostCertificateAuthority: authority }).success, true);
  assert.equal(
    UpdateServerInputSchema.safeParse({
      ...agentAdd,
      serverId: "srv_01abc",
      hostCertificateAuthority: authority,
    }).success,
    true,
  );
  assert.equal(
    UpdateServerInputSchema.safeParse({
      ...agentAdd,
      serverId: "srv_01abc",
      clearHostCertificateAuthority: true,
    }).success,
    true,
  );
  assert.equal(
    UpdateServerInputSchema.safeParse({
      ...agentAdd,
      serverId: "srv_01abc",
      hostCertificateAuthority: authority,
      clearHostCertificateAuthority: true,
    }).success,
    false,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      hostCertificateAuthority: { ...authority, principals: [] },
    }).success,
    false,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      hostCertificateAuthority: { ...authority, revocationListPath: " " },
    }).success,
    false,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      hostCertificateAuthority: {
        ...authority,
        revocationListSigners: Array.from({ length: 9 }, (_, index) => `key-${index}`),
      },
    }).success,
    false,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      hostCertificateAuthority: {
        ...authority,
        revocationListPath: undefined,
        revocationListUrl: "https://ca.example.test/revoked.krl",
      },
    }).success,
    true,
  );
  assert.equal(
    AddServerInputSchema.safeParse({
      ...agentAdd,
      hostCertificateAuthority: {
        ...authority,
        revocationListUrl: "https://ca.example.test/revoked.krl",
      },
    }).success,
    false,
    "local and online KRL sources are mutually exclusive",
  );
});

test("permission decisions always state which layer spoke", () => {
  const parsed = PermissionDecisionSchema.safeParse({
    outcome: "ask",
    intrinsicRisk: "critical",
    finalRisk: "high",
    tier: "dangerous",
    facts: [
      { source: "tool", level: "medium", toolName: "ssh.execute" },
      { source: "command", level: "critical", command: "rm -rf /", matched: ["rm-rf"] },
      { source: "environment", level: "critical", environment: "production" },
    ],
    policyId: "policy.production",
    toolName: "ssh.execute",
    reason: "Command matches rm -rf rule on a production target",
    target: { host: "remote", serverId: "srv_01abc", environment: "production" },
    approvalId: "apr_1",
    requestedAt: "2026-01-01T00:00:00.000Z",
  });
  assert.equal(parsed.success, true);
});

test("permission contracts distinguish Agent delegation from policy approval", () => {
  const parsed = PermissionDecisionSchema.safeParse({
    outcome: "auto",
    intrinsicRisk: "high",
    finalRisk: "high",
    tier: "dangerous",
    facts: [{ source: "tool", level: "high", toolName: "docker.restart" }],
    policyId: "policy.production",
    toolName: "docker.restart",
    reason: "delegated for this run",
    approvedBy: "agent",
    target: { host: "remote", serverId: "srv_01abc", environment: "production" },
    requestedAt: "2026-01-01T00:00:00.000Z",
  });
  assert.equal(parsed.success, true);
});
