// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import { beforeEach, describe, expect, it, vi } from "vitest";
const { localFetch } = vi.hoisted(() => ({ localFetch: vi.fn() }));
vi.mock("@/lib/api", () => ({ localFetch }));
import { explicitMemoryApi, findMarkedFrame, MemoryApiError, normalizedRegion, validateMemoryMark } from "./explicit-memory";
import type { StreamTimeSeriesResponse } from "@/components/rewind/timeline";

beforeEach(() => localFetch.mockReset());
describe("normalized annotation geometry", () => {
  it("keeps the same source rectangle when the displayed image is resized", () => {
    const first = normalizedRegion({ x: 120, y: 70 }, { x: 320, y: 170 }, { left: 20, top: 20, width: 400, height: 200 });
    const second = normalizedRegion({ x: 220, y: 120 }, { x: 620, y: 320 }, { left: 20, top: 20, width: 800, height: 400 });
    expect(first).toEqual({ x: 0.25, y: 0.25, width: 0.5, height: 0.5 });
    expect(second).toEqual(first);
  });
  it("normalizes reverse drags and clamps to source bounds", () => {
    expect(normalizedRegion({ x: 180, y: 100 }, { x: -10, y: -20 }, { left: 0, top: 0, width: 100, height: 100 })).toEqual({ x: 0, y: 0, width: 1, height: 1 });
  });
  it("rejects empty, invalid and click-only selections", () => {
    expect(normalizedRegion({ x: 1, y: 1 }, { x: 1, y: 1 }, { left: 0, top: 0, width: 100, height: 100 })).toBeNull();
    expect(normalizedRegion({ x: NaN, y: 0 }, { x: 50, y: 50 }, { left: 0, top: 0, width: 100, height: 100 })).toBeNull();
    expect(normalizedRegion({ x: 0, y: 0 }, { x: Infinity, y: 50 }, { left: 0, top: 0, width: 100, height: 100 })).toBeNull();
    expect(normalizedRegion({ x: 0, y: 0 }, { x: 10, y: 10 }, { left: 0, top: 0, width: 0, height: 100 })).toBeNull();
  });
});
describe("explicit memory client", () => {
  it("finds the exact monitor containing a marked frame and never picks a nearby frame", () => {
    const frames = [{ timestamp: "2026-10-24T08:00:00Z", devices: [
      { frame_id: "41", device_id: "left" }, { frame_id: "42", device_id: "right" },
    ] }] as StreamTimeSeriesResponse[];
    expect(findMarkedFrame(frames, 42)).toMatchObject({ index: 0, device: { device_id: "right", frame_id: "42" } });
    expect(findMarkedFrame(frames, 43)).toBeNull();
  });
  it("validates UTF-8 byte limits without rejecting short Chinese notes", () => {
    const fields = { title: "增量索引", note: "保存来源证据", tags: ["项目:A"], importance: 0.5, anchors: [] };
    expect(validateMemoryMark(fields)).toBeNull();
    expect(validateMemoryMark({ ...fields, title: "字".repeat(200) })).toContain("title");
    expect(validateMemoryMark({ ...fields, tags: ["字".repeat(40)] })).toContain("tags");
    expect(validateMemoryMark({ ...fields, anchors: Array.from({ length: 20 }, (_, index) => ({ id: String(index), type: "text_quote" as const, quote: "x".repeat(7000), comment: "y".repeat(2000) })) })).toContain("128 KiB");
  });
  it("encodes multilingual search and frame filters", async () => {
    localFetch.mockResolvedValue(new Response(JSON.stringify({ data: [], pagination: { total: 0, offset: 50, limit: 50 } })));
    await explicitMemoryApi.list("增量索引 & Project A", 50, 42);
    const url = new URL(localFetch.mock.calls[0][0], "http://localhost");
    expect(url.searchParams.get("q")).toBe("增量索引 & Project A");
    expect(url.searchParams.get("frame_id")).toBe("42");
    expect(url.searchParams.get("offset")).toBe("50");
  });
  it("fetches image bytes through authenticated localFetch without token URLs", async () => {
    localFetch.mockResolvedValue(new Response("pixels", { headers: { "Content-Type": "image/png" } }));
    const blob = await explicitMemoryApi.image({ markId: "mark-id" });
    expect(blob.size).toBe(6);
    expect(localFetch).toHaveBeenCalledWith("/memory/marks/mark-id/image", expect.objectContaining({ cache: "no-store" }));
    expect(localFetch.mock.calls[0][0]).not.toContain("token=");
  });
  it("never substitutes another frame when the requested image is unavailable", async () => {
    localFetch.mockResolvedValueOnce(new Response("missing", { status: 404 }));
    await expect(explicitMemoryApi.image({ frameId: 42 })).rejects.toMatchObject({ status: 404 });
    expect(localFetch).toHaveBeenCalledWith("/frames/42?fallback=false", expect.objectContaining({ cache: "no-store" }));
    expect(localFetch).toHaveBeenCalledTimes(1);
  });
  it("passes revisions for edit/delete and surfaces conflicts", async () => {
    localFetch.mockResolvedValueOnce(new Response("{}", { status: 409 }));
    await expect(explicitMemoryApi.update("mark-id", 4, { title: "Mine", note: "Note", tags: [], importance: 0.5, anchors: [] })).rejects.toMatchObject({ status: 409 });
    expect(JSON.parse(localFetch.mock.calls[0][1].body).revision).toBe(4);
    localFetch.mockResolvedValueOnce(new Response("{}"));
    await explicitMemoryApi.remove("mark-id", 4);
    expect(localFetch.mock.calls[1]).toEqual(["/memory/marks/mark-id?revision=4", { method: "DELETE" }]);
  });
  it("does not put arbitrary backend errors or note text into user errors", () => {
    expect(new MemoryApiError(503).message).toContain("privacy processing");
    expect(new MemoryApiError(401).message).not.toContain("undefined");
  });
  it("does not promise automatic recovery for privacy-invalidated saved evidence", async () => {
    localFetch.mockResolvedValue(new Response(JSON.stringify({ code: "privacy_revalidation_required", error: "sensitive backend details" }), { status: 503 }));
    await expect(explicitMemoryApi.image({ markId: "mark-id" })).rejects.toMatchObject({ code: "privacy_revalidation_required", message: "Saved evidence is unavailable under the current privacy policy. You can still delete this annotation." });
  });
});
