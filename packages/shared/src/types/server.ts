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
export interface ServerConnection {
  host: string;
  port: number;
  username: string;
  /** Reference to `identities` in SQLite. Secret material stays in the OS store. */
  identityId?: string;
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
  authentication:
    | { method: "password"; password: string }
    | { method: "privateKey"; privateKeyPem: string; passphrase?: string }
    | { method: "identity"; identityId: string };
}

/** Identity row without secrets: it references the OS credential store. */
export interface Identity {
  id: string;
  label: string;
  method: "password" | "privateKey" | "agent";
  credentialRef: string;
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
