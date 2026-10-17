import { useEffect, useState } from "react";

import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { AGENT_NIST_STORAGE_KEY, defaultAgentRoles, loadAgentRoles } from "./agent-nist.js";

export function useAgentRoles() {
  const read = () => {
    try {
      return loadAgentRoles(window.localStorage, `${AGENT_NIST_STORAGE_KEY}${isPreviewFixture() ? ".preview" : ""}`);
    } catch {
      return { roles: defaultAgentRoles(), notice: null };
    }
  };
  const [config, setConfig] = useState(read);
  useEffect(() => {
    const refresh = () => setConfig(read());
    window.addEventListener("agent-role-config-changed", refresh);
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener("agent-role-config-changed", refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);
  return config;
}
