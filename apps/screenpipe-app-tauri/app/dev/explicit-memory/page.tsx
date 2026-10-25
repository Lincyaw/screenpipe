// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useState } from "react";
import { MemoryTools } from "@/components/explicit-memory/memory-tools";
import { MemoryApiError, type ExplicitMemoryApi, type MemoryMark } from "@/lib/explicit-memory";

const stamp = "2026-10-24T08:30:00Z";
const source = { frameId: 700, timestamp: stamp, app: "Browser", text: "Incremental indexing preserves evidence and updates only changed windows." };
const initial: MemoryMark = {
  id: "10cf8d80-d991-4301-9a11-29b92eaf6f51", frame_id: 700, source_frame_id: 700,
  source_timestamp: stamp, source_app: "Browser", source_window: "Design notes — Project A", source_url: "https://example.com/design-notes",
  title: "Incremental indexing for Project A", note: "Use this approach for our local memory index. 用户备注与原始证据分开保存。", tags: ["project:a", "topic:indexing"], importance: 0.9,
  anchors: [{ id: "selection-1", type: "region", x: 0.12, y: 0.35, width: 0.74, height: 0.2, comment: "Only regenerate affected windows." }, { id: "quote-1", type: "text_quote", quote: source.text, comment: "Keep exact source references." }],
  revision: 1, created_at: stamp, updated_at: stamp, source_state: "retained", retained_image: true, image_width: 1000, image_height: 625,
};
const screenshot = `<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="625" viewBox="0 0 1000 625"><rect width="1000" height="625" fill="#f6f6f3"/><rect width="1000" height="54" fill="#e5e5e0"/><text x="40" y="34" font-family="sans-serif" font-size="16" fill="#555">example.com/design-notes · Fictional source</text><text x="120" y="158" font-family="serif" font-size="38" fill="#0a0a0a">A smaller index, a longer memory.</text><text x="120" y="200" font-family="sans-serif" font-size="17" fill="#78786f">PROJECT A / ENGINEERING NOTES</text><text x="120" y="258" font-family="serif" font-size="23" fill="#333">Incremental indexing preserves evidence and updates</text><text x="120" y="296" font-family="serif" font-size="23" fill="#333">only changed windows. Keep original source references,</text><text x="120" y="334" font-family="serif" font-size="23" fill="#333">then regenerate affected ancestors after each edit.</text><path d="M120 380H880" stroke="#ccc"/><text x="120" y="430" font-family="serif" font-size="21" fill="#555">Annotations capture why a moment matters.</text><text x="120" y="468" font-family="serif" font-size="21" fill="#555">The screenshot remains the original evidence.</text><text x="120" y="570" font-family="sans-serif" font-size="14" fill="#78786f">Fictional preview. No captured personal data.</text></svg>`;
let marks = [initial];
const fixture: ExplicitMemoryApi = {
  async list(q = "", offset = 0, frameId) {
    const found = marks.filter((mark) => (frameId === undefined || mark.source_frame_id === frameId) && JSON.stringify(mark).toLowerCase().includes(q.toLowerCase()));
    return { data: found.slice(offset, offset + 50), pagination: { offset, limit: 50, total: found.length } };
  },
  async get(id) { const mark = marks.find((item) => item.id === id); if (!mark) throw new MemoryApiError(404); return mark; },
  async create(body) {
    const existing = marks.find((item) => item.id === body.id); if (existing) return existing;
    const mark = { ...initial, ...body, revision: 1, source_state: body.retain_source_image ? "retained" as const : "available" as const, retained_image: body.retain_source_image, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
    marks = [mark, ...marks]; return mark;
  },
  async update(id, revision, body) {
    const old = await fixture.get(id); if (old.revision !== revision) throw new MemoryApiError(409);
    const mark = { ...old, ...body, revision: revision + 1, updated_at: new Date().toISOString() };
    marks = marks.map((item) => item.id === id ? mark : item); return mark;
  },
  async remove(id, revision) { const old = await fixture.get(id); if (old.revision !== revision) throw new MemoryApiError(409); marks = marks.filter((item) => item.id !== id); },
  async image() { return new Blob([screenshot], { type: "image/svg+xml" }); },
};

export default function ExplicitMemoryPreview() {
  const [openedSource, setOpenedSource] = useState<number | null>(null);
  if (process.env.NEXT_PUBLIC_SCREENPIPE_WEB_DEV !== "mock") return null;
  return <main className="min-h-screen bg-background p-6 text-foreground md:p-12">
    <div className="mx-auto max-w-5xl space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4"><div><p className="text-xs text-muted-foreground">LOCAL MEMORY · FICTIONAL PREVIEW</p><h1 className="mt-2 text-2xl font-medium">Save what matters.</h1></div><MemoryTools source={source} api={fixture} initiallyEnabled onOpenSource={(mark) => setOpenedSource(mark.source_frame_id)} /></div>
      <div className="border border-border bg-[#f6f6f3] p-6 md:p-16 text-[#0a0a0a]"><p className="text-xs text-[#78786f]">PROJECT A / ENGINEERING NOTES</p><h2 className="mt-8 font-serif text-4xl">A smaller index, a longer memory.</h2><p className="my-10 max-w-xl font-serif text-xl leading-relaxed">Incremental indexing preserves evidence and updates only changed windows. Keep original source references, then regenerate affected ancestors after each edit.</p><p className="border-t pt-6 text-sm text-[#78786f]">Frame 700 · Browser · {new Date(stamp).toLocaleString()}</p></div>
      <p className="text-sm text-muted-foreground">Open “My marks” to search, inspect, edit, and delete a saved annotation. “Annotate” opens a frozen source image with region selection.</p>
      {openedSource && <p role="status" className="text-sm">Opened exact source frame {openedSource}.</p>}
    </div>
  </main>;
}
