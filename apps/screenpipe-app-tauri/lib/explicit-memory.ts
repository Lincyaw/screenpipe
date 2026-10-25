// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import { localFetch } from "@/lib/api";
import type { StreamTimeSeriesResponse } from "@/components/rewind/timeline";

export type MemoryAnchor = {
  id: string;
  type: "region" | "text_quote";
  x?: number | null;
  y?: number | null;
  width?: number | null;
  height?: number | null;
  quote?: string | null;
  comment: string;
};
export type MemoryMark = {
  id: string;
  frame_id: number | null;
  source_frame_id: number;
  source_timestamp: string;
  source_app: string | null;
  source_window: string | null;
  source_url: string | null;
  title: string;
  note: string;
  tags: string[];
  importance: number;
  anchors: MemoryAnchor[];
  revision: number;
  created_at: string;
  updated_at: string;
  source_state: "available" | "retained" | "unavailable" | "deleted";
  retained_image: boolean;
  image_width: number | null;
  image_height: number | null;
};
export type MemoryMarkFields = Pick<MemoryMark, "title" | "note" | "tags" | "importance" | "anchors">;
export type MemoryMarkCreate = MemoryMarkFields & { id: string; frame_id: number; retain_source_image: boolean };
export type MemoryMarkPage = { data: MemoryMark[]; pagination: { limit: number; offset: number; total: number } };
export type MemorySource = { frameId: number; timestamp: string; app: string; text?: string };

/** Only supplied after an intentional screenshot has a durable exact frame. */
export type QuickAnnotationCapture = MemorySource & {
  requestId: string;
  width: number;
  height: number;
};
export const EXPLICIT_MEMORY_ENABLED_KEY = "screenpipe:explicit-memory:enabled";

export function hasAnchorRectangle(anchor: MemoryAnchor): anchor is MemoryAnchor & { x: number; y: number; width: number; height: number } {
  return [anchor.x, anchor.y, anchor.width, anchor.height].every((value) => typeof value === "number" && Number.isFinite(value))
    && anchor.x! >= 0 && anchor.y! >= 0 && anchor.width! > 0 && anchor.height! > 0
    && anchor.x! + anchor.width! <= 1 && anchor.y! + anchor.height! <= 1;
}

/** Resolve both frame and monitor; timestamp proximity is never a source match. */
export function findMarkedFrame(frames: StreamTimeSeriesResponse[], frameId: number) {
  for (let index = 0; index < frames.length; index++) {
    const device = frames[index].devices.find((item) => Number(item.frame_id) === frameId);
    if (device) return { index, frame: frames[index], device };
  }
  return null;
}

export class MemoryApiError extends Error {
  constructor(public status: number, public code?: string) {
    super(status === 409
      ? "This annotation changed in another window. Reload it before saving."
      : status === 404 || status === 410
        ? "The source is unavailable or has been deleted."
        : status === 413
          ? "This annotation is too large. Shorten its notes or selections before saving."
        : status === 401 || status === 403
          ? "This action requires local owner access."
          : code === "image_busy"
            ? "The source image is busy. Please try again shortly."
          : code === "privacy_revalidation_required"
            ? "Saved evidence is unavailable under the current privacy policy. You can still delete this annotation."
          : status === 503
            ? "The source is not ready for access. Try again after privacy processing finishes."
            : "Could not complete the annotation request. Please try again.");
    this.name = "MemoryApiError";
  }
}
async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await localFetch(path, init);
  if (!response.ok) {
    let code: string | undefined;
    try {
      const body: unknown = await response.json();
      if (body && typeof body === "object" && "code" in body && typeof body.code === "string") code = body.code;
    } catch { /* Bodies may be absent; never expose arbitrary backend text. */ }
    throw new MemoryApiError(response.status, code);
  }
  return response;
}
const json = (method: string, body: unknown): RequestInit => ({
  method, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
});
export interface ExplicitMemoryApi {
  list(query?: string, offset?: number, frameId?: number): Promise<MemoryMarkPage>;
  create(body: MemoryMarkCreate): Promise<MemoryMark>;
  update(id: string, revision: number, body: MemoryMarkFields): Promise<MemoryMark>;
  remove(id: string, revision: number): Promise<void>;
  get(id: string): Promise<MemoryMark>;
  image(source: { frameId: number } | { markId: string }, signal?: AbortSignal): Promise<Blob>;
}

/** Match server byte limits, including notes and quotes written in CJK scripts. */
export function validateMemoryMark(fields: MemoryMarkFields): string | null {
  const bytes = (text: string) => new TextEncoder().encode(text).byteLength;
  if (bytes(fields.title) > 500) return "The title is too long (maximum 500 UTF-8 bytes).";
  if (bytes(fields.note) > 32000) return "The note is too long (maximum 32,000 UTF-8 bytes).";
  if (fields.tags.length > 32 || fields.tags.some((tag) => bytes(tag) > 100)) return "Use up to 32 tags, each at most 100 UTF-8 bytes.";
  if (fields.anchors.length > 32 || fields.anchors.some((anchor) => bytes(anchor.quote ?? "") > 8000 || bytes(anchor.comment) > 8000)) return "Use up to 32 selections, each quote and comment at most 8,000 UTF-8 bytes.";
  const contentBytes = [fields.title, fields.note, ...fields.tags, ...fields.anchors.flatMap((anchor) => [anchor.quote ?? "", anchor.comment])]
    .reduce((total, text) => total + bytes(text), 0);
  if (contentBytes > 128 * 1024) return "This annotation is too large. Keep all notes, tags and selections within 128 KiB of text.";
  if (bytes(JSON.stringify(fields)) > 250000) return "This annotation is too large. Shorten its notes or selections before saving.";
  return null;
}
export const explicitMemoryApi: ExplicitMemoryApi = {
  async list(query = "", offset = 0, frameId) {
    const params = new URLSearchParams({ q: query, limit: "50", offset: String(offset) });
    if (frameId !== undefined) params.set("frame_id", String(frameId));
    return (await request(`/memory/marks?${params}`)).json();
  },
  async create(body) { return (await request("/memory/marks", json("POST", body))).json(); },
  async update(id, revision, body) {
    return (await request(`/memory/marks/${encodeURIComponent(id)}`, json("PATCH", { ...body, revision }))).json();
  },
  async remove(id, revision) {
    await request(`/memory/marks/${encodeURIComponent(id)}?revision=${revision}`, { method: "DELETE" });
  },
  async get(id) { return (await request(`/memory/marks/${encodeURIComponent(id)}`)).json(); },
  async image(source, signal) {
    const path = "markId" in source
      ? `/memory/marks/${encodeURIComponent(source.markId)}/image`
      : `/frames/${source.frameId}?fallback=false`;
    return (await request(path, { signal, cache: "no-store" })).blob();
  },
};

/** Coordinates refer to image pixels, independently of viewport size or zoom. */
export function normalizedRegion(
  start: { x: number; y: number }, end: { x: number; y: number },
  bounds: { left: number; top: number; width: number; height: number },
): { x: number; y: number; width: number; height: number } | null {
  if (![start.x, start.y, end.x, end.y, bounds.left, bounds.top, bounds.width, bounds.height].every(Number.isFinite)
    || !(bounds.width > 0 && bounds.height > 0)) return null;
  const clamp = (n: number) => Math.min(1, Math.max(0, n));
  const x1 = clamp((start.x - bounds.left) / bounds.width);
  const x2 = clamp((end.x - bounds.left) / bounds.width);
  const y1 = clamp((start.y - bounds.top) / bounds.height);
  const y2 = clamp((end.y - bounds.top) / bounds.height);
  if (![x1, x2, y1, y2].every(Number.isFinite) || Math.abs(x2 - x1) < 0.005 || Math.abs(y2 - y1) < 0.005) return null;
  return { x: Math.min(x1, x2), y: Math.min(y1, y2), width: Math.abs(x2 - x1), height: Math.abs(y2 - y1) };
}
