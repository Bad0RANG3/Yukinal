/** Read-only ContextSource backed by the Rust host's local database. */

import {
  ServerSchema,
  ServerSnapshotSchema,
  WorkspaceSchema,
  type HostContextKind,
  type HostContextResponse,
  type Server,
  type ServerSnapshot,
  type Workspace,
} from "@yukinal/shared";

import type { HostRpcClient } from "../transport/host-client.js";
import type { ContextSource } from "./context-engine.js";

interface Parser<T> {
  parse(value: unknown): T;
}

export function createHostContextSource(client: HostRpcClient): ContextSource {
  return {
    server: (id) => read(client, { kind: "server", id }, ServerSchema),
    snapshot: (id) => read(client, { kind: "snapshot", id }, ServerSnapshotSchema),
    workspace: (id) => read(client, { kind: "workspace", id }, WorkspaceSchema),
  };
}

async function read<T>(
  client: HostRpcClient,
  request: { kind: HostContextKind; id: string },
  parser: Parser<T>,
): Promise<T | undefined> {
  const response: HostContextResponse = await client.fetchContext(request);
  if (response.status === "not_found") return undefined;
  if (response.status === "failed") throw new Error(response.error.message);
  return parser.parse(response.data);
}

export type { Server, ServerSnapshot, Workspace };
