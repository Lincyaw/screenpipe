// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useUiLocale } from "@/lib/i18n/provider";
import { useGT } from "gt-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import {
  explicitMemoryApi, hasAnchorRectangle, MemoryApiError, normalizedRegion, validateMemoryMark,
  type ExplicitMemoryApi, type MemoryAnchor, type MemoryMark, type MemorySource,
} from "@/lib/explicit-memory";

/** Translate only known product errors; never interpret source or user text. */
export function useMemoryErrorText() {
  const ui = useGT();
  return (message: string): string => {
    switch (message) {
      case "Source image unavailable.": return ui("Source image unavailable.");
      case "Could not save annotation.": return ui("Could not save annotation.");
      case "Could not reload annotation.": return ui("Could not reload annotation.");
      case "Could not delete annotation.": return ui("Could not delete annotation.");
      case "Could not load annotations.": return ui("Could not load annotations.");
      case "This annotation changed in another window. Reload it before saving.": return ui("This annotation changed in another window. Reload it before saving.");
      case "The source is unavailable or has been deleted.": return ui("The source is unavailable or has been deleted.");
      case "This annotation is too large. Shorten its notes or selections before saving.": return ui("This annotation is too large. Shorten its notes or selections before saving.");
      case "This action requires local owner access.": return ui("This action requires local owner access.");
      case "The source image is busy. Please try again shortly.": return ui("The source image is busy. Please try again shortly.");
      case "Saved evidence is unavailable under the current privacy policy. You can still delete this annotation.": return ui("Saved evidence is unavailable under the current privacy policy. You can still delete this annotation.");
      case "The source is not ready for access. Try again after privacy processing finishes.": return ui("The source is not ready for access. Try again after privacy processing finishes.");
      case "Could not complete the annotation request. Please try again.": return ui("Could not complete the annotation request. Please try again.");
      case "The title is too long (maximum 500 UTF-8 bytes).": return ui("The title is too long (maximum 500 UTF-8 bytes).");
      case "The note is too long (maximum 32,000 UTF-8 bytes).": return ui("The note is too long (maximum 32,000 UTF-8 bytes).");
      case "Use up to 32 tags, each at most 100 UTF-8 bytes.": return ui("Use up to 32 tags, each at most 100 UTF-8 bytes.");
      case "Use up to 32 selections, each quote and comment at most 8,000 UTF-8 bytes.": return ui("Use up to 32 selections, each quote and comment at most 8,000 UTF-8 bytes.");
      case "This annotation is too large. Keep all notes, tags and selections within 128 KiB of text.": return ui("This annotation is too large. Keep all notes, tags and selections within 128 KiB of text.");
      case "Enable local summaries and select a model in Local summaries.": return ui("Enable local summaries and select a model in Local summaries.");
      case "Choose an installed local model in Local summaries.": return ui("Choose an installed local model in Local summaries.");
      case "This activity is still changing. Choose a closed task or session.": return ui("This activity is still changing. Choose a closed task or session.");
      case "Ollama must run with cloud features disabled before summarizing private evidence.": return ui("Ollama must run with cloud features disabled before summarizing private evidence.");
      case "This item changed. Refresh it before applying your correction.": return ui("This item changed. Refresh it before applying your correction.");
      case "Local owner access is required.": return ui("Local owner access is required.");
      case "This item or its source is no longer available.": return ui("This item or its source is no longer available.");
      case "Check the selected dates, label and model settings.": return ui("Check the selected dates, label and model settings.");
      case "The processing budget is in use. Try again later.": return ui("The processing budget is in use. Try again later.");
      case "This data is not ready. Wait for indexing or privacy processing, then refresh.": return ui("This data is not ready. Wait for indexing or privacy processing, then refresh.");
      case "Could not complete this request. Please try again.": return ui("Could not complete this request. Please try again.");
      case "Could not load local memory. Please refresh.": return ui("Could not load local memory. Please refresh.");
      case "The start date must come before the end date.": return ui("The start date must come before the end date.");
      case "The summary belongs to another activity. Refresh to try again.": return ui("The summary belongs to another activity. Refresh to try again.");
      case "The summary job belongs to another activity. Refresh to try again.": return ui("The summary job belongs to another activity. Refresh to try again.");
      case "The local model could not finish this summary. Check Local summaries and retry.": return ui("The local model could not finish this summary. Check Local summaries and retry.");
      case "Privacy processing is in progress. Memory will reload when protected reads are available.": return ui("Privacy processing is in progress. Memory will reload when protected reads are available.");
      default: return message;
    }
  };
}

const fieldClass = "w-full rounded-md border border-input bg-background px-3 py-2 text-sm";

export function MarkEditor({ source, mark, onClose, onSaved, api = explicitMemoryApi, mode = "historical", createId }: {
  source: MemorySource; mark?: MemoryMark; onClose(): void; onSaved(mark: MemoryMark): void; api?: ExplicitMemoryApi;
  mode?: "historical" | "quickCapture";
  createId?: string;
}) {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const memoryErrorText = useMemoryErrorText();
  const number = (value: number) => value.toLocaleString(uiLocale);
  const quickCapture = mode === "quickCapture";
  const [current, setCurrent] = useState(mark);
  const [title, setTitle] = useState(mark?.title ?? (quickCapture ? (source.app ? ui("Screenshot: {app}", { app: source.app }) : ui("Screenshot")) : ""));
  const [note, setNote] = useState(mark?.note ?? "");
  const [tags, setTags] = useState(mark?.tags.join(", ") ?? "");
  const [importance, setImportance] = useState(mark?.importance ?? 0.5);
  const [anchors, setAnchors] = useState<MemoryAnchor[]>(mark?.anchors ?? []);
  const [quote, setQuote] = useState("");
  const [retain, setRetain] = useState(quickCapture);
  const [image, setImage] = useState<string | null>(null);
  const [imageError, setImageError] = useState<string | null>(null);
  const [imageLoading, setImageLoading] = useState(true);
  const [imageGeneration, setImageGeneration] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  savingRef.current = saving;
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const [confirmDelete, setConfirmDelete] = useState(false);
  // Stable id makes retries of a newly created mark idempotent.
  const requestId = useRef(createId ?? crypto.randomUUID());
  const drag = useRef<{ x: number; y: number } | null>(null);
  const editorRef = useRef<HTMLElement | null>(null);
  const canvasRef = useRef<HTMLElement | null>(null);
  const [canvasBounds, setCanvasBounds] = useState<{ width: number; height: number } | null>(null);
  const [imageDimensions, setImageDimensions] = useState<{ width: number; height: number } | null>(null);
  const [draftRegion, setDraftRegion] = useState<ReturnType<typeof normalizedRegion>>(null);

  useEffect(() => {
    if (quickCapture) editorRef.current?.focus();
  }, [quickCapture]);

  useEffect(() => {
    if (!quickCapture) return;
    const cancel = () => { if (!savingRef.current) closeRef.current(); };
    window.addEventListener("annotation-cancel-request", cancel);
    return () => window.removeEventListener("annotation-cancel-request", cancel);
  }, [quickCapture]);

  useEffect(() => {
    if (!quickCapture || !canvasRef.current) return;
    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      setCanvasBounds((previous) => previous?.width === width && previous.height === height ? previous : { width, height });
    });
    observer.observe(canvasRef.current);
    return () => observer.disconnect();
  }, [quickCapture]);

  const imageScale = canvasBounds && imageDimensions
    ? Math.min(1, canvasBounds.width / imageDimensions.width, canvasBounds.height / imageDimensions.height)
    : null;
  const quickImageSize = imageScale !== null && imageDimensions
    ? { width: Math.max(1, imageDimensions.width * imageScale), height: Math.max(1, imageDimensions.height * imageScale) }
    : undefined;

  useEffect(() => {
    const controller = new AbortController();
    let url: string | null = null;
    setImageLoading(true);
    setImage(null);
    setImageError(null);
    setImageDimensions(null);
    api.image(current ? { markId: current.id } : { frameId: source.frameId }, controller.signal)
      .then((blob) => {
        if (controller.signal.aborted) return;
        url = URL.createObjectURL(blob);
        setImage(url);
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) setImageError(cause instanceof Error ? cause.message : "Source image unavailable.");
      })
      .finally(() => { if (!controller.signal.aborted) setImageLoading(false); });
    return () => { controller.abort(); if (url) URL.revokeObjectURL(url); };
  }, [api, source.frameId, current?.id, imageGeneration]);

  async function save() {
    if (quickCapture && (imageLoading || imageError || !image)) return;
    savingRef.current = true;
    setSaving(true); setError(null);
    const fields = { title: title.trim(), note: note.trim(), tags: [...new Set(tags.split(",").map((tag) => tag.trim()).filter(Boolean))], importance, anchors };
    const validationError = validateMemoryMark(fields);
    if (validationError) { setError(validationError); savingRef.current = false; setSaving(false); return; }
    try {
      const saved = current
        ? await api.update(current.id, current.revision, fields)
        : await api.create({ ...fields, id: requestId.current, frame_id: source.frameId, retain_source_image: retain });
      onSaved(saved);
    } catch (cause) {
      setConflict(cause instanceof MemoryApiError && cause.status === 409);
      setError(cause instanceof Error ? cause.message : "Could not save annotation.");
    } finally { savingRef.current = false; setSaving(false); }
  }
  async function reload() {
    setSaving(true);
    try {
      const fresh = await api.get(current?.id ?? requestId.current);
      setCurrent(fresh); setTitle(fresh.title); setNote(fresh.note); setTags(fresh.tags.join(", "));
      setImportance(fresh.importance); setAnchors(fresh.anchors); setConflict(false); setError(null);
    } catch (cause) { setError(cause instanceof Error ? cause.message : "Could not reload annotation."); }
    finally { setSaving(false); }
  }
  async function remove() {
    if (!current) return;
    setSaving(true);
    try { await api.remove(current.id, current.revision); onClose(); onSaved(current); }
    catch (cause) {
      setConflict(cause instanceof MemoryApiError && cause.status === 409);
      setError(cause instanceof Error ? cause.message : "Could not delete annotation.");
    } finally { setSaving(false); }
  }

  const imageCanvas = <>
          {imageLoading && <p role="status" className="p-6 text-sm">Loading source image…</p>}
          {imageError && <div role="alert" className="space-y-3 border border-border p-6 text-sm"><p>{memoryErrorText(imageError)} Your note is separate from the captured image.</p><Button size="sm" variant="secondary" onClick={() => setImageGeneration((value) => value + 1)}>Retry source image</Button></div>}
          {image && <div className={quickCapture ? "relative max-w-full shrink-0 touch-none select-none cursor-crosshair" : "relative w-full touch-none select-none border border-border cursor-crosshair"}
            style={quickCapture ? quickImageSize : undefined}
            role="img" aria-label={ui("Frozen source screenshot. Drag to select a region.")}
            onPointerDown={(event) => {
              if (event.button !== 0 || anchors.length >= 32) return;
              drag.current = { x: event.clientX, y: event.clientY };
              setDraftRegion(null);
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerMove={(event) => {
              if (!drag.current) return;
              const bounds = event.currentTarget.querySelector("img")?.getBoundingClientRect();
              if (bounds) setDraftRegion(normalizedRegion(drag.current, { x: event.clientX, y: event.clientY }, bounds));
            }}
            onPointerCancel={() => { drag.current = null; setDraftRegion(null); }}
            onPointerUp={(event) => {
              if (!drag.current) return;
              const imageBounds = event.currentTarget.querySelector("img")?.getBoundingClientRect();
              const region = imageBounds ? normalizedRegion(drag.current, { x: event.clientX, y: event.clientY }, imageBounds) : null;
              drag.current = null;
              setDraftRegion(null);
              if (region) setAnchors((items) => [...items, { id: crypto.randomUUID(), type: "region", ...region, comment: "" }]);
            }}>
            {/* The img determines the wrapper's exact aspect ratio; no letterboxing. */}
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src={image} alt={ui("Original captured screen")} draggable={false} className={quickCapture && quickImageSize ? "block h-full w-full" : "block w-full h-auto"} onLoad={(event) => setImageDimensions({ width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight })} />
            {draftRegion && <div className="pointer-events-none absolute border-2 border-dashed border-white bg-black/15 outline outline-1 outline-black" style={{ left: `${draftRegion.x * 100}%`, top: `${draftRegion.y * 100}%`, width: `${draftRegion.width * 100}%`, height: `${draftRegion.height * 100}%` }} />}
            {anchors.map((anchor, index) => hasAnchorRectangle(anchor) ? <div key={anchor.id} data-testid={`anchor-box-${anchor.id}`} className="absolute border-2 border-white bg-black/15 pointer-events-none outline outline-1 outline-black"
              style={{ left: `${anchor.x * 100}%`, top: `${anchor.y * 100}%`, width: `${anchor.width * 100}%`, height: `${anchor.height * 100}%` }}>
              <span className="absolute left-0 top-0 bg-black px-1 text-xs text-white">{number(index + 1)}</span>
            </div> : null)}
          </div>}
  </>;
  const quoteFields = <>
          <label className="block text-sm">Quoted text
            <textarea aria-label={ui("Quoted text")} maxLength={16000} className={`${fieldClass} mt-1 h-20`} value={quote} onChange={(event) => setQuote(event.target.value)} placeholder={ui("Paste or type the exact passage you want to save")} />
          </label>
          <p className="text-xs text-muted-foreground">Quotes are saved as user-selected text, without claiming a stable OCR or accessibility element link.</p>
          <Button variant="secondary" size="sm" disabled={!quote.trim() || anchors.length >= 32} onClick={() => {
            setAnchors((items) => [...items, { id: crypto.randomUUID(), type: "text_quote", quote: quote.trim(), comment: "" }]); setQuote("");
          }}>Add quoted text</Button>
  </>;
  const selectedRegions = <>
          {anchors.map((anchor, index) => <div key={anchor.id} className="rounded-md border border-border p-3 space-y-2">
            <div className="flex items-center justify-between gap-2 text-xs"><span>{number(index + 1)}. {anchor.type === "region" ? ui("Selected region") : ui("Quoted text")}</span><button type="button" className="underline" onClick={() => setAnchors((items) => items.filter((item) => item.id !== anchor.id))}>{ui("Remove selection {number}", { number: number(index + 1) })}</button></div>
            {anchor.quote && <blockquote className="break-words border-l-2 pl-2 text-sm">{anchor.quote}</blockquote>}
            <input aria-label={ui("Selection {number} comment", { number: number(index + 1) })} maxLength={4000} className={fieldClass} placeholder={ui("Why this part matters")} value={anchor.comment} onChange={(event) => setAnchors((items) => items.map((item) => item.id === anchor.id ? { ...item, comment: event.target.value } : item))} />
          </div>)}
  </>;
  const detailFields = <>

          <label className="block text-sm">Title<input aria-label={ui("Annotation title")} maxLength={500} className={`${fieldClass} mt-1`} value={title} onChange={(event) => setTitle(event.target.value)} /></label>
          <label className="block text-sm">Your note<textarea aria-label={ui("Your note")} maxLength={16000} className={`${fieldClass} mt-1 min-h-36`} value={note} onChange={(event) => setNote(event.target.value)} placeholder={ui("What matters about this moment?")} /></label>
          <label className="block text-sm">Tags<input aria-label={ui("Annotation tags")} maxLength={2000} className={`${fieldClass} mt-1`} value={tags} onChange={(event) => setTags(event.target.value)} placeholder={ui("project:a, topic:indexing")} /></label>
          <label className="block text-sm">Importance: {new Intl.NumberFormat(uiLocale, { style: "percent" }).format(importance)}<input aria-label={ui("Importance")} type="range" min="0" max="1" step="0.1" className="mt-2 w-full accent-foreground" value={importance} onChange={(event) => setImportance(Number(event.target.value))} /></label>
          {!current && <label className="flex items-start gap-2 text-sm" title={quickCapture ? ui("New captures keep their evidence with the saved note by default.") : undefined}><input type="checkbox" checked={retain} disabled={imageLoading || !!imageError} onChange={(event) => setRetain(event.target.checked)} /><span>Keep source image after ordinary history cleanup. Explicit source deletion still removes saved evidence.</span></label>}
          {current && <p className="text-xs text-muted-foreground">{current.retained_image ? ui("Source image retained with this annotation.") : ui("Source image follows ordinary history cleanup.")} Your comment is separate from the captured evidence.</p>}
  </>;
  const saveError = <>
      {error && <div role="alert" className="rounded-md border border-border p-3 text-sm">{memoryErrorText(error)}{conflict && <Button size="sm" variant="secondary" className="ml-3" disabled={saving} onClick={reload}>Reload saved version</Button>}</div>}
  </>;
  const actions = (
      <div className={quickCapture ? "flex shrink-0 flex-wrap items-center justify-between gap-3 border-t bg-background px-4 py-3" : "sticky bottom-0 z-10 flex flex-wrap items-center justify-between gap-3 border-t bg-background py-3"}>
        <div>{current && (!confirmDelete ? <Button size="sm" variant="ghost" disabled={saving} onClick={() => setConfirmDelete(true)}>Delete annotation</Button> : <span className="flex flex-wrap items-center gap-2 text-sm">Remove note and its retained image?<Button size="sm" variant="destructive" disabled={saving} onClick={remove}>Confirm delete</Button><Button size="sm" variant="ghost" onClick={() => setConfirmDelete(false)}>Keep it</Button></span>)}</div>
        <div className="flex gap-2"><Button variant="secondary" disabled={saving} onClick={onClose}>Cancel</Button><Button onClick={save} disabled={saving || conflict || (quickCapture && (imageLoading || !!imageError || !image)) || (!title.trim() && !note.trim() && anchors.length === 0)}>{saving ? ui("Saving…") : ui("Save annotation")}</Button></div>
      </div>
  );

  if (quickCapture) return <main
    ref={editorRef}
    tabIndex={-1}
    className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground outline-none"
    onKeyDown={(event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (!saving) onClose();
      }
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
        event.preventDefault();
        if (!saving && !conflict && (title.trim() || note.trim() || anchors.length)) void save();
      }
    }}
  >
    <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
      <div><h1 className="text-sm font-medium">Capture and annotate</h1><p className="mt-0.5 text-xs text-muted-foreground">{source.app || ui("Captured screen")} · {new Date(source.timestamp).toLocaleString(uiLocale)}</p></div>
      <p className="text-xs text-muted-foreground">Drag to select · Esc to cancel · ⌘/Ctrl Enter to save</p>
    </header>
    <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
      <section ref={canvasRef} aria-label={ui("Frozen screenshot")} className="flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-auto bg-muted/40 p-3">
        {imageCanvas}
      </section>
      <aside aria-label={ui("Annotation details")} className="max-h-[45vh] shrink-0 space-y-4 overflow-y-auto border-t p-4 lg:max-h-none lg:w-80 lg:border-l lg:border-t-0">
        {detailFields}
        {selectedRegions}
        <details className="space-y-3"><summary className="cursor-pointer text-sm">Add quoted text</summary><div className="space-y-3 pt-3">{quoteFields}</div></details>
        {saveError}
      </aside>
    </div>
    {actions}
  </main>;

  return <Dialog open onOpenChange={(open) => { if (!open && !saving) onClose(); }}>
    <DialogContent data-native-timeline-occluder="true" className="max-w-5xl max-h-[90vh] overflow-y-auto select-text" onPointerDown={(event) => event.stopPropagation()} onWheel={(event) => event.stopPropagation()}>
      <DialogHeader>
        <DialogTitle>{current ? ui("Edit annotation") : ui("Annotate this moment")}</DialogTitle>
        <DialogDescription>{source.app || ui("Captured screen")} · {new Date(source.timestamp).toLocaleString(uiLocale)} · Frame {source.frameId}</DialogDescription>
      </DialogHeader>
      <div className="grid gap-6 md:grid-cols-[minmax(0,1.5fr)_minmax(240px,1fr)]">
        <div className="min-w-0 space-y-3">
          <p className="text-xs text-muted-foreground">Drag on the frozen image to mark a region. The original stays unchanged.</p>
          {imageCanvas}
          {quoteFields}
          {selectedRegions}
        </div>
        <div className="space-y-4">{detailFields}</div>
      </div>
      {saveError}
      {actions}
    </DialogContent>
  </Dialog>;
}
