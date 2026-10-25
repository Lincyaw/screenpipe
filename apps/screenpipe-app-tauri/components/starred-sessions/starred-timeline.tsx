// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import { useGT } from "gt-react";
import { useUiLocale } from "@/lib/i18n/provider";
import { useCallback, useEffect, useState } from "react";
import { Star } from "lucide-react";
import { emit } from "@tauri-apps/api/event";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { useTauriEvent } from "@/lib/hooks/use-tauri-event";
import { useStarredSessions } from "./use-starred-sessions";
import { SessionHistory } from "./session-history";
import { StarredSessionPanel } from "./starred-session-panel";

export function StarredTimeline({
  showStrip = false,
}: {
  showStrip?: boolean;
}) {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const number = (value: number) => value.toLocaleString(uiLocale);
  const state = useStarredSessions();
  const [open, setOpen] = useState(false);
  const dismiss = useCallback(() => setOpen(false), []);
  useEffect(() => {
    if (showStrip) return;
    const openControls = () => setOpen(true);
    const url = new URL(window.location.href);
    if (url.searchParams.get("starred") === "1") {
      setOpen(true);
      url.searchParams.delete("starred");
      window.history.replaceState(window.history.state, "", url);
    }
    window.addEventListener("open-starred-sessions", openControls);
    return () =>
      window.removeEventListener("open-starred-sessions", openControls);
  }, [showStrip]);
  useTauriEvent<{ url: string }>("navigate", (event) => {
    if (
      !showStrip &&
      new URL(event.payload.url, window.location.origin).searchParams.get(
        "starred",
      ) === "1"
    )
      setOpen(true);
  });
  return (
    <>
      {showStrip && (
        <div
          aria-label={ui("Starred moments")}
          className="flex shrink-0 items-center gap-2 overflow-hidden border-b px-3 py-2 text-xs"
        >
          <button
            className="flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1.5 hover:bg-muted"
            disabled={!state.ready || state.busy}
            onClick={async () => {
              if (!state.active) await state.start(60);
              window.dispatchEvent(new Event("open-starred-sessions"));
            }}
          >
            <Star
              className={`h-3.5 w-3.5 ${state.active ? "fill-current" : ""}`}
            />
            {state.active ? ui("{count} min left", { count: number(Math.max(1, Math.ceil((Date.parse(state.active.end) - state.now) / 60000))) }) : ui("Star session")}
          </button>
          <div className="hidden min-w-0 flex-1 items-center gap-1 overflow-hidden sm:flex">
          {state.sessions.slice(0, 3).map((s) => (
            <button
              key={s.id}
              title={ui("{start} to {end}", { start: new Date(s.start).toLocaleString(uiLocale), end: new Date(s.end).toLocaleString(uiLocale) })}
              className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1.5 text-muted-foreground hover:bg-muted"
              onClick={() => void emit("navigate-to-timestamp", s.start)}
            >
              <Star className="h-3 w-3 fill-current" />
              {new Date(s.start).toLocaleDateString(uiLocale, {
                month: "short",
                day: "numeric",
              })}{" "}
              {new Date(s.start).toLocaleTimeString(uiLocale, {
                hour: "2-digit",
                minute: "2-digit",
              })}
            </button>
          ))}
          </div>
          <SessionHistory />
        </div>
      )}
      {!showStrip && (
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogContent aria-describedby={undefined} className="max-h-[85vh] max-w-sm overflow-y-auto border-white/20 bg-black p-0 text-white">
            <DialogTitle className="sr-only">Starred sessions</DialogTitle>
            <StarredSessionPanel state={state} inDialog onDismiss={dismiss} />
          </DialogContent>
        </Dialog>
      )}
    </>
  );
}
