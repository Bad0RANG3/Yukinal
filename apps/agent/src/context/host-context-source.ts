/** Read-only ContextSource backed by the Rust host's local database. */

// 这里只导入 schema：三个行类型的**收窄**由 `parser.parse()` 的返回值完成，
// 函数签名上并不出现具体类型名。曾经末尾有一行 `export type { Server, … }`
// 把三个类型再转发一次，但全仓库没有任何地方从本模块导入它们（只有两处导入
// `createHostContextSource`），所以那行连同它撑着的三条 type 导入一起删掉。
import {
  ServerSchema,
  ServerSnapshotSchema,
  WorkspaceSchema,
  type HostContextKind,
  type HostContextResponse,
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
