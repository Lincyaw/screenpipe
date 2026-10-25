// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useGT } from "gt-react";
import { useUiLocale } from "@/lib/i18n/provider";
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { MarkEditor } from "@/components/explicit-memory/mark-editor";
import { Button } from "@/components/ui/button";
import { EXPLICIT_MEMORY_ENABLED_KEY, type MemoryMark, type QuickAnnotationCapture } from "@/lib/explicit-memory";
import { commands } from "@/lib/utils/tauri";

function isCapture(value: unknown): value is QuickAnnotationCapture {
  if (!value || typeof value !== "object") return false;
  const capture = value as Partial<QuickAnnotationCapture>;
  return typeof capture.requestId === "string"
    && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(capture.requestId)
    && typeof capture.frameId === "number" && Number.isSafeInteger(capture.frameId) && capture.frameId > 0
    && typeof capture.timestamp === "string" && Number.isFinite(Date.parse(capture.timestamp))
    && typeof capture.app === "string"
    && typeof capture.width === "number" && Number.isSafeInteger(capture.width) && capture.width > 0
    && typeof capture.height === "number" && Number.isSafeInteger(capture.height) && capture.height > 0;
}

export default function AnnotatePage() {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const [capture, setCapture] = useState<QuickAnnotationCapture | null>(null);
  const captureRef = useRef<QuickAnnotationCapture | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [closing, setClosing] = useState(false);
  const [saved, setSaved] = useState<MemoryMark | null>(null);
  const [reload, setReload] = useState(0);
  const closeInFlightRef = useRef(false);
  const editorActiveRef = useRef(false);
  editorActiveRef.current = Boolean(capture && !saved);

  const acceptCapture = useCallback((value: unknown) => {
    if (!isCapture(value)) {
      setError(ui("The captured screenshot is unavailable. Retry or close this window."));
      setLoading(false);
      return;
    }
    // A new shortcut must not replace an unsaved draft in a reused webview.
    if (captureRef.current && captureRef.current.requestId !== value.requestId) return;
    captureRef.current = value;
    setCapture(value);
    setLoading(false);
    setError(null);
  }, [uiLocale]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    setLoading(true);
    setError(null);
    void (async () => {
      // Listen before reading state so a reused window cannot miss completion.
      try {
        unlisten = await listen<QuickAnnotationCapture>("annotation-capture-ready", async ({ payload }) => {
          if (cancelled || !isCapture(payload)) return;
          // Completion notifications are hints. Read the committed native state
          // before trusting the frame, including for a reused window.
          try {
            const result = await commands.getAnnotationCapture();
            if (!cancelled && result.status === "ok" && isCapture(result.data) && result.data.requestId === payload.requestId) acceptCapture(result.data);
          } catch {
            if (!cancelled && !captureRef.current) setError(ui("The screenshot could not be opened. Retry or close this window."));
          }
        });
      } catch {
        // The committed-state command is sufficient for the normal new window.
      }
      if (cancelled) { unlisten?.(); return; }
      try {
        const result = await commands.getAnnotationCapture();
        if (cancelled) return;
        if (result.status === "ok" && result.data) acceptCapture(result.data);
        else if (!captureRef.current) {
          setLoading(false);
          setError(ui("No captured screenshot is ready. Retry or close this window."));
        }
      } catch {
        if (!cancelled && !captureRef.current) {
          setLoading(false);
          setError(ui("The screenshot could not be opened. Retry or close this window."));
        }
      }
    })();
    return () => { cancelled = true; unlisten?.(); };
  }, [acceptCapture, reload]);

  const closeCapture = useCallback(async () => {
    if (closeInFlightRef.current) return;
    closeInFlightRef.current = true;
    setClosing(true);
    setError(null);
    try {
      const current = captureRef.current;
      if (current) {
        const result = await commands.closeAnnotationCapture(current.requestId);
        if (result.status === "error") throw new Error("close failed");
      } else await getCurrentWindow().close();
    } catch {
      closeInFlightRef.current = false;
      setError(ui("This window could not be closed. Please try again."));
      setClosing(false);
    }
  }, [uiLocale]);

  useEffect(() => {
    let cancelled = false;
    const subscription = listen("escape-pressed", () => {
      if (cancelled) return;
      if (editorActiveRef.current) window.dispatchEvent(new Event("annotation-cancel-request"));
      else void closeCapture();
    });
    return () => { cancelled = true; void subscription.then((unlisten) => unlisten()).catch(() => {}); };
  }, [closeCapture]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      // The editor owns Escape while a draft is visible, including save state.
      if (event.key !== "Escape" || (capture && !saved)) return;
      event.preventDefault();
      void closeCapture();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [capture, closeCapture, saved]);

  if (capture && !saved) return <>
    <MarkEditor
      key={capture.requestId}
      mode="quickCapture"
      createId={capture.requestId}
      source={capture}
      onClose={() => void closeCapture()}
      onSaved={(mark) => {
        try { window.localStorage.setItem(EXPLICIT_MEMORY_ENABLED_KEY, "true"); } catch { /* Saving does not depend on local preferences. */ }
        setSaved(mark);
        void closeCapture();
      }}
    />
    {error && <div role="alert" className="fixed inset-x-4 bottom-20 z-50 border border-border bg-background p-3 text-sm text-foreground">{error}</div>}
    {closing && <div role="status" className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 text-sm text-foreground">Closing…</div>}
  </>;

  return <main className="flex h-screen w-screen items-center justify-center bg-background p-6 text-foreground">
    <div className="max-w-md space-y-4 text-center">
      <h1 className="text-lg font-medium">{saved ? ui("Annotation saved") : ui("Capture and annotate")}</h1>
      {saved && <p className="text-sm text-muted-foreground">{saved.retained_image ? ui("Saved locally, with its source image.") : ui("Saved locally.")}</p>}
      {loading && !saved && <p role="status" className="text-sm text-muted-foreground">Opening the captured screenshot…</p>}
      {error && <p role="alert" className="text-sm text-muted-foreground">{error}</p>}
      <div className="flex justify-center gap-2">
        {error && !saved && <Button variant="secondary" disabled={closing} onClick={() => setReload((value) => value + 1)}>Retry</Button>}
        <Button disabled={closing} onClick={() => void closeCapture()}>{closing ? ui("Closing…") : ui("Close")}</Button>
      </div>
    </div>
  </main>;
}
