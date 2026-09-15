import { IPC_SCHEMAS } from "./src/schemas/ipc.ts";
const run = IPC_SCHEMAS.agent_run_start.params;
const clip = { type: "audio", mediaType: "audio/wav", data: "aGVsbG8=", name: "note.wav" };
const r = run.safeParse({ sessionId: "ses_1", prompt: "", parts: [clip] });
console.log(JSON.stringify(r.success ? "ok" : r.error.issues, null, 1));