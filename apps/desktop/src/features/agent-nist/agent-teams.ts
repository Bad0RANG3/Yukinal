/**
 * The team vocabulary that survives the retirement of role routing.
 *
 * There used to be a role catalog here: seven teams, thirteen roles, a persisted
 * assignment per role, a model binding per role, an @mention router and a store
 * field (`agentRoleId`) that selected which persona the next run used. The runtime
 * became a single operations Agent — one persona, one model, subagents launched on
 * demand — so the picker, the router and the handoff were removed. What remains is
 * exactly what persisted data still needs:
 *
 *   - **the id set**, because tasks and conversations saved before the change carry
 *     a team id and must keep rendering a label rather than nothing;
 *   - **one label**, because every one of those ids now names the same single Agent.
 *
 * This module is deliberately inert: it has no storage, no provider access, no
 * `window`, and no store. A label lookup that can read localStorage or a provider
 * catalog is how the old routing came back last time.
 */

/** Team ids that may appear in persisted tasks and archived conversations. */
export const HISTORICAL_AGENT_TEAMS = [
  { id: "blue", label: "蓝队", englishName: "Blue Team" },
  { id: "red", label: "红队", englishName: "Red Team" },
  { id: "purple", label: "紫队", englishName: "Purple Team" },
  { id: "white", label: "白队", englishName: "White Team" },
  { id: "yellow", label: "黄队", englishName: "Yellow Team" },
  { id: "orange", label: "橙队", englishName: "Orange Team" },
  { id: "green", label: "绿队", englishName: "Green Team" },
] as const;

/** The three work kinds the retired catalog split a conversation across. */
export const AGENT_TEAMS = [
  { id: "yellow", label: "执行", englishName: "Builder" },
  { id: "blue", label: "运维", englishName: "Operations" },
  { id: "purple", label: "验证", englishName: "Reviewer" },
] as const;

export type AgentTeam = (typeof HISTORICAL_AGENT_TEAMS)[number]["id"];

const KNOWN_TEAM_IDS: ReadonlySet<string> = new Set([
  ...AGENT_TEAMS.map((team) => team.id),
  ...HISTORICAL_AGENT_TEAMS.map((team) => team.id),
]);

/**
 * What a stored team id reads as now: the one visible Agent.
 *
 * An unknown id returns `undefined` rather than a guess, so a caller can decide
 * whether to fall back to its own noun instead of being handed a wrong label.
 */
export function agentTeamLabel(team: string): string | undefined {
  return KNOWN_TEAM_IDS.has(team) ? "运维 Agent" : undefined;
}
