// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { useGT } from "gt-react";
import { useUiLocale } from "@/lib/i18n/provider";
import { useEffect, useRef, useState } from "react";
import { Star } from "lucide-react";
import { emit } from "@tauri-apps/api/event";
import { localFetch } from "@/lib/api";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import type { StarredSession } from "./use-starred-sessions";

export function SessionHistory() {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const number = (value: number) => value.toLocaleString(uiLocale);
  const errorText = (message: string) => message === "Could not load sessions." ? ui("Could not load sessions.") : message;
  const [open, setOpen] = useState(false);
  const [day, setDay] = useState("");
  const [rows, setRows] = useState<StarredSession[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const cursor = useRef(0);
  const epoch = useRef(0);
  const requestPending = useRef(false);
  const snapshot = useRef(new Date().toISOString());
  async function load(reset: boolean) {
    if (requestPending.current && !reset) return;
    const token = reset ? ++epoch.current : epoch.current;
    if (reset) {
      cursor.current = 0;
      snapshot.current = new Date().toISOString();
      setRows([]);
    }
    requestPending.current = true;
    setBusy(true);
    setError(undefined);
    try {
      const params = new URLSearchParams({
        limit: "20",
        offset: String(cursor.current),
        end_time: snapshot.current,
      });
      if (day) {
        const start = new Date(`${day}T00:00:00`);
        const end = new Date(start);
        end.setDate(end.getDate() + 1);
        params.set("start_time", start.toISOString());
        params.set("end_time", end.toISOString());
      }
      const response = await localFetch(`/starred-sessions?${params}`);
      const result = await response.json();
      if (!response.ok || !Array.isArray(result.data))
        throw new Error(result.error || "Could not load sessions.");
      if (token !== epoch.current) return;
      const next: StarredSession[] = result.data;
      cursor.current += next.length;
      setHasMore(next.length === 20);
      setRows((previous) => [
        ...new Map([...previous, ...next].map((row) => [row.id, row])).values(),
      ]);
    } catch (e) {
      if (token === epoch.current)
        setError(e instanceof Error ? e.message : "Could not load sessions.");
    } finally {
      if (token === epoch.current) {
        requestPending.current = false;
        setBusy(false);
      }
    }
  }
  useEffect(() => {
    if (open) void load(true);
    return () => {
      ++epoch.current;
      requestPending.current = false;
    };
  }, [open, day]);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button className="ml-auto shrink-0 rounded-md border px-2 py-1.5 hover:bg-muted">
          View all
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 max-w-[calc(100vw-2rem)] p-0">
        <div className="border-b p-3">
          <h3 className="mb-2 text-sm font-medium">Starred sessions</h3>
          <label className="flex items-center gap-2 text-xs">
            Date{" "}
            <input
              type="date"
              aria-label={ui("Filter sessions by date")}
              value={day}
              onChange={(e) => setDay(e.target.value)}
              className="min-w-0 flex-1 rounded border bg-background p-1.5"
            />
          </label>
          {day && (
            <button
              className="mt-2 text-xs underline"
              onClick={() => setDay("")}
            >
              All dates
            </button>
          )}
        </div>
        <div className="max-h-[min(320px,50vh)] overflow-y-auto p-2">
          {rows.map((session, index) => (
            <div key={session.id}>
              {(index === 0 ||
                new Date(rows[index - 1].start).toDateString() !==
                  new Date(session.start).toDateString()) && (
                <p className="px-2 pb-1 pt-2 text-[10px] text-muted-foreground">
                  {new Date(session.start).toLocaleDateString(uiLocale, {
                    month: "short",
                    day: "numeric",
                    year: "numeric",
                  })}
                </p>
              )}
              <button
                className="flex w-full items-center gap-2 rounded-md p-2 text-left text-xs hover:bg-muted"
                onClick={() => {
                  void emit("navigate-to-timestamp", session.start);
                  setOpen(false);
                }}
              >
                <Star className="h-3 w-3 fill-current" />
                <span>
                  {new Date(session.start).toLocaleTimeString(uiLocale, {
                    hour: "2-digit",
                    minute: "2-digit",
                  })}
                  <span className="block pt-1 text-[10px] text-muted-foreground">
                    {ui("{count} min", { count: number(Math.max(
                      1,
                      Math.round(
                        (Date.parse(session.end) - Date.parse(session.start)) /
                          60000,
                      ),
                    )) })}{" "}
                    {session.has_audio ? ui(" · Audio") : ""}
                  </span>
                </span>
              </button>
            </div>
          ))}
          {!rows.length && !busy && !error && (
            <p className="p-4 text-xs text-muted-foreground">
              {day ? ui("No starred sessions on this date.") : ui("No starred sessions yet.")}
            </p>
          )}
          {error && (
            <p role="alert" className="p-2 text-xs">
              {errorText(error)}{" "}
              <button className="underline" onClick={() => void load(false)}>
                Retry
              </button>
            </p>
          )}
          {busy ? (
            <p role="status" className="p-2 text-xs">
              Loading sessions…
            </p>
          ) : (
            hasMore &&
            !error && (
              <button
                className="mt-2 w-full rounded border p-2 text-xs hover:bg-muted"
                onClick={() => void load(false)}
              >
                Load more
              </button>
            )
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
