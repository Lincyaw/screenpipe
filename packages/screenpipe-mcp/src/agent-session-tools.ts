// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import type { Tool } from "@modelcontextprotocol/sdk/types.js";

const sources = ["codex", "claude", "cursor", "gemini"];
const sourceProperty = { type: "string", enum: sources, description: "Local agent transcript source." };
export const AGENT_SESSION_TOOLS: Tool[] = [
  {
    name: "search-agent-sessions",
    description: "Search bounded local Codex, Claude, Cursor and Gemini session transcripts as historical context. Does not start, resume or message agents. Preserve coverage warnings; recorded assistant claims do not prove completion.",
    inputSchema: { type: "object", properties: {
      q: { type: "string", maxLength: 1000, description: "Text to match in transcript, title or workspace; omit for recent sessions." },
      source: sourceProperty,
      limit: { type: "integer", minimum: 1, maximum: 50, default: 20 },
    }, additionalProperties: false },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  },
  {
    name: "read-agent-session",
    description: "Read a bounded text excerpt from a local agent session returned by search-agent-sessions. Historical evidence only; follow the returned coverage limits. Does not continue the conversation.",
    inputSchema: { type: "object", properties: {
      source: sourceProperty,
      id: { type: "string", minLength: 1, maxLength: 200, description: "Exact session ID from search-agent-sessions." },
    }, required: ["source", "id"], additionalProperties: false },
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  },
];

export async function readAgentSessionTool(
  name: string,
  args: Record<string, unknown>,
  callAPI: (endpoint: string) => Promise<Response>,
) {
  const params = new URLSearchParams();
  if (args.source !== undefined) {
    if (typeof args.source !== "string" || !sources.includes(args.source)) throw new Error("Invalid session source");
    params.set("source", args.source);
  }
  if (name === "read-agent-session") {
    if (!params.has("source") || typeof args.id !== "string" || !args.id.trim() || args.id.length > 200 || /[/\\]/.test(args.id)) throw new Error("Use a source and exact session ID returned by search-agent-sessions");
    params.set("id", args.id);
  } else if (name === "search-agent-sessions") {
    if (args.q !== undefined) {
      if (typeof args.q !== "string" || new TextEncoder().encode(args.q).length > 1000) throw new Error("q must be text of at most 1000 bytes");
      params.set("q", args.q);
    }
    if (args.limit !== undefined) {
      if (typeof args.limit !== "number" || !Number.isSafeInteger(args.limit) || args.limit < 1 || args.limit > 50) throw new Error("limit must be between 1 and 50");
      params.set("limit", String(args.limit));
    }
  } else throw new Error("Unknown session tool");
  const response = await callAPI(`/agent-sessions?${params}`);
  if (!response.ok) throw new Error(`Session context unavailable (HTTP ${response.status}); this is not an empty result`);
  return { content: [{ type: "text" as const, text: JSON.stringify(await response.json()) }] };
}
