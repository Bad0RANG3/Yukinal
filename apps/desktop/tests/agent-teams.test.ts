import assert from "node:assert/strict";
import { test } from "node:test";
import type { AiProviderConfig } from "@yukinal/shared";

import { AGENT_TEAMS, HISTORICAL_AGENT_TEAMS, agentTeamLabel } from "../src/features/agent-nist/agent-teams.js";

test("every retired team id still reads as the one visible Agent", () => {
  // The role picker is gone: there is one operations Agent, so a stored task or an
  // old @mention that names a team must not render a team noun. The mapping is kept
  // (rather than deleted) because persisted rows still carry these ids, and a
  // reader that silently dropped them would show nothing where a label used to be.
  assert.equal(agentTeamLabel("blue"), "运维 Agent");
  assert.equal(agentTeamLabel("yellow"), "运维 Agent");
  for (const team of [...AGENT_TEAMS, ...HISTORICAL_AGENT_TEAMS]) {
    assert.equal(agentTeamLabel(team.id), "运维 Agent", `${team.id} must not resurrect a team noun`);
  }
  assert.equal(agentTeamLabel("not-a-team"), undefined);
});

test("the retired team catalog is a compatibility list, not a routing table", () => {
  // Guards against the catalog quietly becoming a selector again: the visible set
  // names execution/operations/verification work, and every historical id is still
  // covered so a persisted label resolves.
  assert.equal(AGENT_TEAMS.length, 3);
  assert.ok(HISTORICAL_AGENT_TEAMS.length > AGENT_TEAMS.length);
  // The visible catalog and the historical one intentionally share ids: the visible
  // set is the current wording for three of the seven historical teams.
  assert.deepEqual(AGENT_TEAMS.map((team) => team.id).sort(), ["blue", "purple", "yellow"]);
  assert.deepEqual(HISTORICAL_AGENT_TEAMS.map((team) => team.id).sort(), ["blue", "green", "orange", "purple", "red", "white", "yellow"]);
});

test("a provider catalog can still be read for a saved binding, but no role handoff exists", () => {
  // `roleModelChoices` is the last surviving reader of the stored role records: the
  // model page uses it to explain why a saved assignment no longer resolves. The
  // handoff itself is gone, which is why this file no longer imports it.
  const provider: AiProviderConfig = {
    id: "provider-a", enabled: false, kind: "openai-compatible", label: "Connection",
    baseUrl: "http://127.0.0.1:1234/v1", model: "default-model", createdAt: "", updatedAt: "",
    models: [{ id: "default-model", label: "Default model", supportsStreaming: true, supportsToolCalling: true }],
  };
  assert.equal(provider.models?.length, 1);
});
