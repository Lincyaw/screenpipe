// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { describe, expect, it, vi } from "vitest";
import { AGENT_SESSION_TOOLS, readAgentSessionTool } from "./agent-session-tools";

describe("agent session context", () => {
  it("exposes only read-only search and retrieval", () => {
    expect(AGENT_SESSION_TOOLS.map(tool => tool.name)).toEqual(["search-agent-sessions", "read-agent-session"]);
    expect(AGENT_SESSION_TOOLS.every(tool => tool.annotations?.readOnlyHint)).toBe(true);
  });
  it("searches the local API and preserves coverage warnings", async () => {
    const data = { results: [{ source: "codex", id: "thread-1", can_send: false }], warnings: ["bounded scan"], has_more: true };
    const api = vi.fn(async (_endpoint: string) => Response.json(data));
    const result = await readAgentSessionTool("search-agent-sessions", { q: "capture & recovery", source: "codex", limit: 5 }, api);
    const endpoint = new URL(api.mock.calls[0][0] as string, "http://localhost");
    expect(endpoint.pathname).toBe("/agent-sessions");
    expect(endpoint.searchParams.get("q")).toBe("capture & recovery");
    expect(JSON.parse(result.content[0].text)).toEqual(data);
  });
  it("reads only an exact source and id and reports API errors", async () => {
    const api = vi.fn(async () => Response.json({ error: "restricted history" }, { status: 403 }));
    await expect(readAgentSessionTool("read-agent-session", { source: "codex", id: "thread-1" }, api)).rejects.toThrow("403");
    expect(api).toHaveBeenCalledWith("/agent-sessions?source=codex&id=thread-1");
  });
  it("rejects paths, unknown sources and oversized searches before access", async () => {
    const api = vi.fn();
    for (const args of [{ source: "codex", id: "../secrets" }, { source: "screenpipe", id: "thread-1" }, { id: "thread-1" }]) {
      await expect(readAgentSessionTool("read-agent-session", args, api)).rejects.toThrow();
    }
    await expect(readAgentSessionTool("search-agent-sessions", { q: "x".repeat(1001) }, api)).rejects.toThrow();
    await expect(readAgentSessionTool("search-agent-sessions", { limit: 51 }, api)).rejects.toThrow();
    expect(api).not.toHaveBeenCalled();
  });
});
