// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useUiLocale } from "@/lib/i18n/provider";
import { useGT } from "gt-react";
import { useEffect, useRef, useState } from "react";
import { BookmarkPlus, BookOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { EXPLICIT_MEMORY_ENABLED_KEY as ENABLED_KEY, explicitMemoryApi, type ExplicitMemoryApi, type MemoryMark, type MemorySource } from "@/lib/explicit-memory";
import { MarkEditor, useMemoryErrorText } from "./mark-editor";

export function MemoryTools({ source, onOpenSource, api = explicitMemoryApi, initiallyEnabled = false }: {
  source: MemorySource | null;
  onOpenSource(mark: MemoryMark): void | Promise<void>;
  api?: ExplicitMemoryApi;
  initiallyEnabled?: boolean;
}) {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const memoryErrorText = useMemoryErrorText();
  const number = (value: number) => value.toLocaleString(uiLocale);
  const [enabled, setEnabled] = useState(initiallyEnabled);
  const [open, setOpen] = useState(false);
  const [editor, setEditor] = useState<{ source: MemorySource; mark?: MemoryMark } | null>(null);
  const [query, setQuery] = useState("");
  const [frameOnly, setFrameOnly] = useState(false);
  const [marks, setMarks] = useState<MemoryMark[]>([]);
  const [offset, setOffset] = useState(0);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generation, setGeneration] = useState(0);
  const [navigationError, setNavigationError] = useState<string | null>(null);
  const [openingSource, setOpeningSource] = useState<string | null>(null);
  const searchInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    try { setEnabled(initiallyEnabled || window.localStorage.getItem(ENABLED_KEY) === "true"); } catch { /* Opt-in remains off when storage is unavailable. */ }
    const sync = (event: StorageEvent) => {
      if (event.key === ENABLED_KEY) setEnabled(initiallyEnabled || event.newValue === "true");
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, [initiallyEnabled]);
  useEffect(() => {
    if (!enabled || !open) return;
    let active = true;
    setLoading(true); setError(null);
    const timer = setTimeout(() => {
      api.list(query, offset, frameOnly && source ? source.frameId : undefined)
        .then((page) => { if (active) { setMarks(page.data); setTotal(page.pagination.total); } })
        .catch((cause) => { if (active) setError(cause instanceof Error ? cause.message : "Could not load annotations."); })
        .finally(() => { if (active) setLoading(false); });
    }, query ? 200 : 0);
    return () => { active = false; clearTimeout(timer); };
  }, [api, enabled, open, query, offset, frameOnly, source?.frameId, generation]);

  if (!enabled) return <Button size="sm" variant="secondary" title={ui("Enable local screenshot annotations")} onClick={() => {
    try { window.localStorage.setItem(ENABLED_KEY, "true"); } catch { /* Current session can still opt in. */ }
    setEnabled(true);
  }}><BookmarkPlus className="mr-1 h-3 w-3" />Enable annotations</Button>;

  return <>
    <div className="flex flex-wrap items-center gap-1" data-testid="explicit-memory-tools">
      <Button size="sm" variant="secondary" disabled={!source || openingSource !== null} onClick={() => source && setEditor({ source: { ...source } })}><BookmarkPlus className="mr-1 h-3 w-3" />Annotate</Button>
      <Button size="sm" variant="secondary" disabled={openingSource !== null} onClick={() => setOpen(true)}><BookOpen className="mr-1 h-3 w-3" />My marks</Button>
      {openingSource && <span role="status" className="text-xs text-muted-foreground">Opening exact source…</span>}
    </div>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent data-native-timeline-occluder="true" className="max-w-2xl max-h-[85vh] overflow-y-auto select-text" onPointerDown={(event) => event.stopPropagation()} onWheel={(event) => event.stopPropagation()}>
        <DialogHeader><DialogTitle>My marks</DialogTitle><DialogDescription>Your notes and selections, linked to their original moments.</DialogDescription></DialogHeader>
        <div className="flex flex-wrap items-center gap-3">
          <input ref={searchInput} aria-label={ui("Search my marks")} type="search" value={query} onChange={(event) => { setQuery(event.target.value); setOffset(0); }} className="min-w-40 flex-1 rounded-md border border-input bg-background px-3 py-2 text-sm" placeholder={ui("Search notes, selected text, or tags")} />
          {source && <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={frameOnly} onChange={(event) => { setFrameOnly(event.target.checked); setOffset(0); }} />This frame only</label>}
        </div>
        {loading && <p role="status" className="py-4 text-sm">Searching annotations…</p>}
        {error && <p role="alert" className="border border-border p-3 text-sm">{memoryErrorText(error)}<Button size="sm" variant="ghost" onClick={() => setGeneration((value) => value + 1)}>Retry</Button></p>}
        {navigationError && <p role="alert" className="text-sm">{ui("Could not open the exact source frame. Your saved annotation is still available.")}</p>}
        {!loading && !error && marks.length === 0 && <p className="py-10 text-center text-sm text-muted-foreground">{query ? ui("No matching marks. Try another phrase.") : ui("No marks yet. Annotate a moment in your timeline to save what matters.")}</p>}
        {!loading && !error && marks.map((mark) => <article key={mark.id} className="space-y-3 rounded-lg border border-border p-4">
          <div><p className="text-xs text-muted-foreground">Your note · {new Date(mark.source_timestamp).toLocaleString(uiLocale)} · {mark.source_app || ui("Captured screen")}</p><h3 className="mt-1 font-medium break-words">{mark.title || ui("Untitled annotation")}</h3></div>
          {mark.note && <p className="whitespace-pre-wrap break-words text-sm">{mark.note}</p>}
          {mark.anchors.filter((anchor) => anchor.quote).slice(0, 2).map((anchor) => <blockquote key={anchor.id} className="border-l-2 pl-3 text-sm text-muted-foreground break-words">{anchor.quote}</blockquote>)}
          {mark.tags.length > 0 && <p className="text-xs text-muted-foreground break-words">{mark.tags.join(" · ")}</p>}
          <div className="flex flex-wrap items-center gap-2">
            <span className="mr-auto text-xs text-muted-foreground">{mark.retained_image ? ui("Source image retained") : mark.source_state === "unavailable" || mark.source_state === "deleted" ? ui("Source unavailable") : ui("Original source")} · {ui("{count, plural, one {# selection} other {# selections}}", { count: mark.anchors.length })}</span>
            <Button size="sm" variant="secondary" onClick={() => { setOpen(false); setEditor({ source: { frameId: mark.source_frame_id, timestamp: mark.source_timestamp, app: mark.source_app || "" }, mark }); }}>Open annotation</Button>
            {mark.frame_id !== null && <Button size="sm" variant="ghost" disabled={openingSource !== null} onClick={async () => {
              setNavigationError(null);
              setOpeningSource(mark.id);
              setOpen(false);
              try { await onOpenSource(mark); }
              catch { setNavigationError("Could not open the exact source frame. Your saved annotation is still available."); setOpen(true); }
              finally { setOpeningSource(null); }
            }}>{openingSource === mark.id ? ui("Opening exact source…") : ui("View in timeline")}</Button>}
          </div>
        </article>)}
        {!loading && !error && total > 50 && <div className="flex items-center justify-between text-sm"><Button size="sm" variant="secondary" disabled={offset === 0} onClick={() => setOffset((value) => Math.max(0, value - 50))}>Previous page</Button><span>{ui("{from}–{to} of {total}", { from: number(offset + 1), to: number(Math.min(offset + 50, total)), total: number(total) })}</span><Button size="sm" variant="secondary" disabled={offset + 50 >= total} onClick={() => setOffset((value) => value + 50)}>Next page</Button></div>}
        <div className="flex items-center justify-between border-t pt-3"><p className="text-xs text-muted-foreground">Saved locally. No AI processing is required.</p><Button size="sm" variant="ghost" onClick={() => {
          try { window.localStorage.removeItem(ENABLED_KEY); } catch { /* Session state remains authoritative. */ }
          setOpen(false); setEnabled(false);
        }}>Disable annotations</Button></div>
      </DialogContent>
    </Dialog>
    {editor && <MarkEditor key={editor.mark?.id ?? `new-${editor.source.frameId}`} source={editor.source} mark={editor.mark} api={api} onClose={() => setEditor(null)} onSaved={() => { setEditor(null); setGeneration((value) => value + 1); setOpen(true); }} />}
  </>;
}
