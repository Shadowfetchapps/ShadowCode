import { useEffect, useState } from "react";
import { api, type AgentInfo } from "../api";

/** The partial agent name while the message is just `@name` (the same
 * rule the slash menu uses for `/name`), otherwise null. */
export function mentionQuery(task: string): string | null {
  const match = /^@([A-Za-z0-9_-]*)$/.exec(task);
  return match ? match[1] : null;
}

/** Agent definitions for the `@` menu, loaded the first time it opens. */
export function useAgentOptions(active: boolean): AgentInfo[] {
  const [agents, setAgents] = useState<AgentInfo[] | null>(null);
  useEffect(() => {
    if (!active || agents) return;
    let live = true;
    api
      .agents()
      .then((result) => {
        if (live) setAgents(result.agents);
      })
      .catch(() => {
        if (live) setAgents([]);
      });
    return () => {
      live = false;
    };
  }, [active, agents]);
  return agents || [];
}
