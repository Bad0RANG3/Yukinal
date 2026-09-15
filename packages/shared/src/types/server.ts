/**
 * Server model.
 *
 * A server is NOT `host + port + username`. It is an addressable environment with
 * capabilities, an identity, and an environment class that drives permissions.
 */

import type { Environment } from "./risk.js";

export const SERVER_STATUSES = ["connecting", "connected", "disconnected", "error"] as const;
export type ServerStatus = (typeof SERVER_STATUSES)[number];

/** — filled in by the Collector engine via detect(), never hand-typed by users. */
export interface ServerCapabilities {
  linux?: boolean;
  docker?: boolean;
  systemd?: boolean;
  nginx?: boolean;
  postgres?: boolean;
  redis?: boolean;
  kubernetes?: boolean;
}

/** — `identityId` points at an *identity row*, which points at a credential_ref. */
export interface HostCertificateAuthority {
  /** OpenSSH public-key line for the CA that signs host certificates. */
  caPublicKey: string;
  /** Allowed certificate principals; `*` and `?` wildcards are supported. */
  principals: string[];
  /** Optional local OpenSSH KRL whose revoked certificates/keys are rejected. */
  revocationListPath?: string;
  /** Optional HTTPS OpenSSH KRL URL; mutually exclusive with the local path. */
  revocationListUrl?: string;
  /** Public keys trusted to sign the KRL independently of the host CA. */
  revocationListSigners?: string[];
}

export interface ServerConnection {
  host: string;
  port: number;
  username: string;
  /** Reference to `identities` in SQLite. Secret material stays in the OS store. */
  identityId?: string;
  /** Optional explicit OpenSSH host-certificate trust root. */
  hostCertificateAuthority?: HostCertificateAuthority;
}

export interface ServerMetadata {
  /** Drives permission policy + UI identity (). */
  environment: Environment;
  /** e.g. "Singapore" — always rendered next to the name in production. */
  region?: string;
  hostname?: string;
  os?: string;
  /** Free-form labels used by Context Engine filtering. */
  tags?: string[];
  /** Which workspace(s) this server belongs to. */
  workspaceIds?: string[];
}

export interface Server {
  /** Stable, generated id. All tool calls must resolve to this. */
  id: string;
  name: string;
  connection: ServerConnection;
  groupId?: string;
  capabilities: ServerCapabilities;
  status: ServerStatus;
  metadata: ServerMetadata;
  createdAt: string;
  updatedAt: string;
}

/** Fields the "Add Server" form actually submits (— advanced options hidden). */
export interface AddServerInput {
  name: string;
  host: string;
  port?: number;
  username: string;
  environment: Environment;
  groupId?: string;
  hostCertificateAuthority?: HostCertificateAuthority;
  authentication:
    | { method: "password"; password: string }
    | { method: "privateKey"; privateKeyPem: string; passphrase?: string }
    | {
        method: "certificate";
        privateKeyPem: string;
        passphrase?: string;
        /** Explicit certificate path. Public material; safe to persist. */
        certificatePath: string;
        /** Optional origin path of the private key, for diagnostics or sibling conventions. */
        privateKeyPath?: string;
      }
    /** ssh-agent：**不携带任何 secret** —— 身份在 agent 手里，Yukinal 不落凭据。 */
    | { method: "agent" }
    | { method: "identity"; identityId: string };
}

/** Editable server fields. Authentication is optional so an edit can retain
 * the current keychain credential without exposing it to the UI.
 *
 * `method: "agent"` is the exception: it never carries a secret, so "nothing was
 * typed" cannot mean "keep the stored credential" — selecting the agent *is* the
 * instruction, and it must be sent every time. */
export interface UpdateServerInput {
  serverId: string;
  name: string;
  host: string;
  port?: number;
  username: string;
  environment: Environment;
  groupId?: string;
  hostCertificateAuthority?: HostCertificateAuthority;
  clearHostCertificateAuthority?: boolean;
  authentication?:
    | { method: "password"; password: string }
    | { method: "privateKey"; privateKeyPem: string; passphrase?: string }
    | {
        method: "certificate";
        privateKeyPem: string;
        passphrase?: string;
        certificatePath: string;
        privateKeyPath?: string;
      }
    | { method: "agent" }
    | { method: "identity"; identityId: string };
}

/** Identity row without secrets: it references the OS credential store. */
export interface Identity {
  id: string;
  label: string;
  method: "password" | "privateKey" | "certificate" | "agent";
  /** `""` for `agent`: that identity has no credential entry at all. */
  credentialRef: string;
  /** Reference to the passphrase entry of an encrypted private key. A reference,
   * never the passphrase itself — and absent for a plaintext key, a password, or
   * an agent identity. */
  passphraseRef?: string;
  /** OpenSSH public certificate path for certificate identities. */
  certificatePath?: string;
  /** Original private-key path, used only for diagnostics and sibling conventions. */
  privateKeyPath?: string;
  createdAt: string;
}

export interface ServerGroup {
  id: string;
  name: string;
  serverIds: string[];
}

/** — a workspace is what the user talks about ("E-commerce Production"). */
export interface Workspace {
  id: string;
  name: string;
  serverIds: string[];
  /** Local or remote repository paths (phase 2). */
  repositories: WorkspaceRepository[];
  /** Infrastructure provider ids (phase 2+). */
  providerIds: string[];
  /** Default environment used when the user does not name one. */
  defaultEnvironment: Environment;
}

export interface WorkspaceListResponse {
  workspaces: Workspace[];
}

export interface WorkspaceRepository {
  id: string;
  name: string;
  /** "local" or "remote" — never guess, mis-targeting a repo is a real incident. */
  host: "local" | "remote";
  path?: string;
  serverId?: string;
  gitUrl?: string;
  defaultBranch?: string;
}
