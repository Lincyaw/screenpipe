// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
const mocks = vi.hoisted(() => ({ localFetch: vi.fn() }));
vi.mock("@/lib/api", () => ({ localFetch: mocks.localFetch }));
import { AgentSessionContext } from "./agent-session-context";
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const session = { source: "codex", id: "fixture", title: "Capture recovery", preview: "Investigate the backlog", updated_at: 1 };
describe("agent session records", () => {
  it("reads an excerpt without a composer or session continuation", async () => {
    mocks.localFetch.mockImplementation(async (path: string) => ({ ok: true, json: async () => path.includes("id=")
      ? { session, text: "Keep recordings durable", warnings: [], max_lines: 5000, max_characters: 60000 }
      : { results: [session], warnings: ["Older records were not searched"], has_more: false } }));
    render(<AgentSessionContext />);
    fireEvent.click(await screen.findByRole("button", { name: /Capture recovery/ }));
    expect(await screen.findByText("Keep recordings durable")).toBeVisible();
    expect(screen.getByText(/It may not include the end/)).toBeVisible();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("button", { name: /send|continue|resume/i })).toBeNull();
    expect(mocks.localFetch).toHaveBeenLastCalledWith("/agent-sessions?source=codex&id=fixture", expect.anything());
    fireEvent.click(screen.getByRole("button", { name: "Back to sessions" }));
    expect(await screen.findByText("Older records were not searched")).toBeVisible();
  });
  it("shows failed access instead of an empty-history claim", async () => {
    mocks.localFetch.mockResolvedValue({ ok: false, status: 403 });
    render(<AgentSessionContext />);
    expect(await screen.findByRole("alert")).toHaveTextContent("403");
    expect(screen.queryByText(/No matching local sessions/)).toBeNull();
  });
  it("searches the record text without importing it into chat storage", async () => {
    mocks.localFetch.mockResolvedValue({ ok: true, json: async () => ({ results: [], warnings: [], has_more: false }) });
    render(<AgentSessionContext />);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "capture backlog" } });
    await waitFor(() => expect(mocks.localFetch).toHaveBeenCalledWith("/agent-sessions?q=capture+backlog&limit=30", expect.anything()));
    expect(await screen.findByText(/No matching local sessions/)).toBeVisible();
  });
});
