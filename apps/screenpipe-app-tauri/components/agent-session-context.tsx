// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useEffect, useState } from "react";
import { localFetch } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

type Session = { source: string; id: string; title: string; preview: string; updated_at: number; workspace?: string };
type SearchResult = { results: Session[]; warnings: string[]; has_more: boolean };
type Detail = { session: Session; text: string; coverage: string; max_lines: number; max_characters: number; warnings: string[] };

/** Sessions are reference records, never chat destinations. */
export function AgentSessionContext() {
  const [query, setQuery] = useState("");
  const [result, setResult] = useState<SearchResult | null>(null);
  const [selected, setSelected] = useState<Session | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true); setError(""); setDetail(null); setResult(null);
    const params = selected
      ? new URLSearchParams({ source: selected.source, id: selected.id })
      : new URLSearchParams({ q: query, limit: "30" });
    const timer = setTimeout(() => {
      void localFetch(`/agent-sessions?${params}`, { signal: controller.signal }).then(async response => {
        if (!response.ok) throw new Error(`Could not read local sessions (${response.status}).`);
        const data = await response.json();
        if (controller.signal.aborted) return;
        if (selected) setDetail(data); else setResult(data);
      }).catch(error => { if (!controller.signal.aborted) setError(String(error.message)); })
        .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    }, selected ? 0 : 250);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [query, selected, revision]);
  return <section className="h-full overflow-auto px-6 pb-8 pt-12" aria-label="Agent session records">
    <header className="mb-6 flex items-center justify-between gap-3">
      <div><h1 className="text-lg font-medium">Agent sessions</h1><p className="mt-1 text-sm text-muted-foreground">Local conversation records for context.</p></div>
      <Button variant="outline" size="sm" onClick={() => setRevision(value => value + 1)}>Refresh</Button>
    </header>
    {selected ? <Button variant="ghost" onClick={() => setSelected(null)}>Back to sessions</Button>
      : <Input aria-label="Search agent sessions" placeholder="Search local sessions" value={query} onChange={event => setQuery(event.target.value)} />}
    {loading && <p role="status" className="mt-4 text-sm text-muted-foreground">Reading local records…</p>}
    {error && <p role="alert" className="mt-4 text-sm">{error}</p>}
    {(result?.warnings ?? detail?.warnings ?? []).map(warning => <p key={warning} className="mt-3 text-xs text-muted-foreground">{warning}</p>)}
    {result && <div className="mt-4 divide-y divide-border">
      {result.results.map(session => <button key={`${session.source}:${session.id}`} className="block w-full py-4 text-left hover:bg-muted/30" onClick={() => setSelected(session)}>
        <div className="text-xs text-muted-foreground">{session.source} · {new Date(session.updated_at).toLocaleString()}</div>
        <h2 className="mt-1 text-sm font-medium">{session.title}</h2><p className="mt-1 text-xs text-muted-foreground">{session.preview}</p>
      </button>)}
      {!result.results.length && <p className="py-6 text-sm text-muted-foreground">No matching local sessions found in the bounded scan.</p>}
      {result.has_more && <p className="py-4 text-xs text-muted-foreground">More sessions matched. Narrow your search.</p>}
    </div>}
    {detail && <article className="mt-4"><h2 className="text-base font-medium">{detail.session.title}</h2>
      <p className="mt-2 text-xs text-muted-foreground">Historical excerpt, limited to {detail.max_lines} source lines and {detail.max_characters} text characters. It may not include the end of this session.</p>
      <pre className="mt-4 whitespace-pre-wrap break-words font-sans text-sm leading-6">{detail.text}</pre>
    </article>}
  </section>;
}
