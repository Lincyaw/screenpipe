// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { expect, it } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { createServer } from "node:http";
import { execFileSync } from "node:child_process";
import path from "node:path";

it("discovers and reads session context through the built stdio MCP", async () => {
  const root = path.resolve(__dirname, "..");
  execFileSync("bun", ["run", "build"], { cwd: root, stdio: "pipe", timeout: 120000 });
  const requests: string[] = [];
  const api = createServer((request, response) => {
    requests.push(request.url ?? "");
    if (request.headers.authorization !== "Bearer fixture-key") { response.writeHead(403); response.end(); return; }
    const url = new URL(request.url!, "http://localhost");
    if (url.pathname === "/agent-sessions") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify(url.searchParams.has("id")
        ? { session: { source: "codex", id: "fixture" }, text: "Preserve durable recording", coverage: "bounded_prefix", historical: true }
        : { results: [{ source: "codex", id: "fixture", can_send: false }], warnings: ["bounded scan"] }));
    } else { response.writeHead(404); response.end(); }
  });
  await new Promise<void>(resolve => api.listen(0, "127.0.0.1", resolve));
  const port = (api.address() as { port: number }).port;
  const transport = new StdioClientTransport({ command: "node", args: [path.join(root, "dist/index.js"), "--port", String(port)], env: { ...process.env, SCREENPIPE_API_KEY: "fixture-key", SCREENPIPE_DISABLE_TELEMETRY: "1" } });
  const client = new Client({ name: "session-context-eval", version: "1" });
  try {
    await client.connect(transport);
    const tools = await client.listTools();
    expect(tools.tools.map(tool => tool.name)).toEqual(expect.arrayContaining(["search-agent-sessions", "read-agent-session"]));
    const searched = await client.callTool({ name: "search-agent-sessions", arguments: { q: "recording", source: "codex" } });
    expect(searched.isError).not.toBe(true);
    expect(JSON.stringify(searched)).toContain("bounded scan");
    const read = await client.callTool({ name: "read-agent-session", arguments: { source: "codex", id: "fixture" } });
    expect(read.isError).not.toBe(true);
    expect(JSON.stringify(read)).toContain("Preserve durable recording");
    expect(requests.filter(request => request.startsWith("/agent-sessions"))).toHaveLength(2);
    expect(requests.some(request => /resume|queue|send/.test(request))).toBe(false);
  } finally {
    await client.close(); await transport.close();
    await new Promise<void>(resolve => api.close(() => resolve()));
  }
}, 120000);
