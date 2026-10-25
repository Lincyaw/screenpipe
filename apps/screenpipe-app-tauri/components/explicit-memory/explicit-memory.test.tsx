// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryTools } from "./memory-tools";
import { MarkEditor } from "./mark-editor";
import { MemoryApiError, type ExplicitMemoryApi, type MemoryMark } from "@/lib/explicit-memory";

const source = { frameId: 42, timestamp: "2026-10-24T08:00:00Z", app: "Browser" };
const mark: MemoryMark = {
  id: "mark-1", frame_id: 42, source_frame_id: 42, source_timestamp: source.timestamp,
  source_app: "Browser", source_window: "Notes", source_url: null,
  title: "Indexing idea", note: "Project A 增量索引", tags: ["project:a"], importance: 0.8,
  anchors: [], revision: 3, created_at: source.timestamp, updated_at: source.timestamp,
  source_state: "retained", retained_image: true, image_width: 1000, image_height: 600,
};
function fakeApi(): ExplicitMemoryApi {
  return {
    list: vi.fn().mockResolvedValue({ data: [mark], pagination: { total: 1, offset: 0, limit: 50 } }),
    create: vi.fn().mockResolvedValue(mark), update: vi.fn().mockResolvedValue(mark),
    remove: vi.fn().mockResolvedValue(undefined), get: vi.fn().mockResolvedValue(mark),
    image: vi.fn().mockResolvedValue(new Blob(["image"], { type: "image/png" })),
  };
}
beforeEach(() => {
  window.localStorage.clear();
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: vi.fn(() => "blob:source"), revokeObjectURL: vi.fn() }));
});

describe("explicit memory opt-in and retrieval", () => {
  it("makes no requests until explicitly enabled and opened", async () => {
    const api = fakeApi();
    render(<MemoryTools source={source} api={api} onOpenSource={vi.fn()} />);
    expect(api.list).not.toHaveBeenCalled();
    expect(api.image).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Enable annotations" }));
    expect(api.list).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "My marks" }));
    expect(await screen.findByText("Project A 增量索引")).toBeVisible();
    expect(api.list).toHaveBeenCalledWith("", 0, undefined);
    fireEvent.click(screen.getByRole("checkbox", { name: "This frame only" }));
    await waitFor(() => expect(api.list).toHaveBeenCalledWith("", 0, 42));
  });
  it("opens the exact saved source rather than selecting another frame", async () => {
    const api = fakeApi(); const onOpenSource = vi.fn();
    render(<MemoryTools source={source} api={api} initiallyEnabled onOpenSource={onOpenSource} />);
    fireEvent.click(screen.getByRole("button", { name: "My marks" }));
    const button = await screen.findByRole("button", { name: "View in timeline" });
    await act(async () => { fireEvent.click(button); });
    expect(onOpenSource).toHaveBeenCalledWith(mark);
  });
  it("waits for exact-source confirmation before closing the marks drawer", async () => {
    const api = fakeApi();
    let finish: () => void = () => {};
    const onOpenSource = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<MemoryTools source={source} api={api} initiallyEnabled onOpenSource={onOpenSource} />);
    fireEvent.click(screen.getByRole("button", { name: "My marks" }));
    fireEvent.click(await screen.findByRole("button", { name: "View in timeline" }));
    expect(screen.getByRole("button", { name: "Opening exact source…" })).toBeDisabled();
    expect(screen.getByRole("dialog")).toBeVisible();
    await act(async () => { finish(); });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
describe("frozen annotation editor", () => {
  it("does not render null quote geometry and preserves mixed selection numbering", async () => {
    const api = fakeApi();
    render(<MarkEditor source={source} mark={{ ...mark, anchors: [
      { id: "quote", type: "text_quote", quote: "Quoted first", comment: "", x: null, y: null, width: null, height: null },
      { id: "region", type: "region", comment: "", x: 0.2, y: 0.3, width: 0.4, height: 0.2 },
    ] }} api={api} onClose={vi.fn()} onSaved={vi.fn()} />);
    await screen.findByAltText("Original captured screen");
    expect(screen.queryByTestId("anchor-box-quote")).not.toBeInTheDocument();
    expect(screen.getByTestId("anchor-box-region")).toHaveTextContent("2");
    expect(screen.getByLabelText("Selection 2 comment")).toBeVisible();
  });
  it("saves a note and quoted passage with explicit image retention", async () => {
    const api = fakeApi(); const onSaved = vi.fn();
    render(<MarkEditor source={source} api={api} onClose={vi.fn()} onSaved={onSaved} />);
    await screen.findByAltText("Original captured screen");
    fireEvent.change(screen.getByLabelText("Annotation title"), { target: { value: "Useful idea" } });
    fireEvent.change(screen.getByLabelText("Your note"), { target: { value: "Use this in Project A" } });
    fireEvent.change(screen.getByLabelText("Quoted text"), { target: { value: "Only reindex affected windows." } });
    fireEvent.click(screen.getByRole("button", { name: "Add quoted text" }));
    fireEvent.change(screen.getByLabelText("Selection 1 comment"), { target: { value: "Why it matters" } });
    fireEvent.change(screen.getByLabelText("Annotation tags"), { target: { value: "project:a, topic:index, project:a" } });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Save annotation" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(mark));
    expect(api.create).toHaveBeenCalledWith(expect.objectContaining({
      frame_id: 42, title: "Useful idea", note: "Use this in Project A", retain_source_image: true,
      tags: ["project:a", "topic:index"], anchors: [expect.objectContaining({ type: "text_quote", quote: "Only reindex affected windows.", comment: "Why it matters" })],
    }));
  });
  it("blocks overwrite after a revision conflict until the saved version is loaded", async () => {
    const api = fakeApi(); vi.mocked(api.update).mockRejectedValueOnce(new MemoryApiError(409));
    render(<MarkEditor source={source} mark={mark} api={api} onClose={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Your note"), { target: { value: "My pending edit" } });
    fireEvent.click(screen.getByRole("button", { name: "Save annotation" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("changed in another window");
    expect(screen.getByRole("button", { name: "Save annotation" })).toBeDisabled();
    expect(screen.getByLabelText("Your note")).toHaveValue("My pending edit");
    fireEvent.click(screen.getByRole("button", { name: "Reload saved version" }));
    await waitFor(() => expect(screen.getByLabelText("Your note")).toHaveValue(mark.note));
    expect(screen.getByRole("button", { name: "Save annotation" })).toBeEnabled();
  });
  it("recovers a new mark whose create response was lost before a changed retry", async () => {
    const api = fakeApi();
    vi.mocked(api.create).mockRejectedValueOnce(new MemoryApiError(409));
    render(<MarkEditor source={source} api={api} onClose={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Your note"), { target: { value: "Changed retry" } });
    fireEvent.click(screen.getByRole("button", { name: "Save annotation" }));
    await screen.findByRole("alert");
    const id = vi.mocked(api.create).mock.calls[0][0].id;
    fireEvent.click(screen.getByRole("button", { name: "Reload saved version" }));
    await waitFor(() => expect(api.get).toHaveBeenCalledWith(id));
    await waitFor(() => expect(screen.getByLabelText("Your note")).toHaveValue(mark.note));
    expect(screen.getByRole("button", { name: "Save annotation" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete annotation" })).toBeEnabled();
  });
  it("keeps the note usable when the exact source image no longer exists", async () => {
    const api = fakeApi(); vi.mocked(api.image).mockRejectedValueOnce(new MemoryApiError(410));
    render(<MarkEditor source={source} mark={{ ...mark, frame_id: null, source_state: "unavailable" }} api={api} onClose={vi.fn()} onSaved={vi.fn()} />);
    expect(await screen.findByText(/source is unavailable/)).toBeVisible();
    expect(screen.getByLabelText("Your note")).toHaveValue(mark.note);
    expect(screen.queryByAltText("Original captured screen")).not.toBeInTheDocument();
    expect(api.image).toHaveBeenCalledWith({ markId: "mark-1" }, expect.any(AbortSignal));
  });
  it("requires confirmation before deleting the mark and retained image", async () => {
    const api = fakeApi(); const onSaved = vi.fn();
    render(<MarkEditor source={source} mark={mark} api={api} onClose={vi.fn()} onSaved={onSaved} />);
    fireEvent.click(screen.getByRole("button", { name: "Delete annotation" }));
    expect(api.remove).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm delete" }));
    await waitFor(() => expect(api.remove).toHaveBeenCalledWith("mark-1", 3));
    expect(onSaved).toHaveBeenCalled();
  });
});
