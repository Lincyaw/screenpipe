// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useUiLocale } from "@/lib/i18n/provider";
import { useGT } from "gt-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { ArrowLeft, ChevronRight, Loader2, RefreshCw, Search, Settings2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { MarkEditor, useMemoryErrorText } from "@/components/explicit-memory/mark-editor";
import { explicitMemoryApi, type MemoryMark } from "@/lib/explicit-memory";
import { useTimelineStore } from "@/lib/hooks/use-timeline-store";
import {
  hierarchicalMemoryApi as api, HierarchyApiError, type HierarchySummary, type MemoryNode, type MemoryNodeDetail,
  type MemoryEvidence, type MemoryIndexStatus, type MemorySearchHit, type SummaryConfig,
  type SummaryJob, type SummaryStatus, type MemoryExportManifest,
} from "@/lib/hierarchical-memory";

const field = "rounded-md border border-input bg-background px-3 py-2 text-sm min-w-0";
type MemoryTab = "tree" | "apps" | "marks" | "search";
const isTreeTab = (tab: MemoryTab) => tab === "tree" || tab === "apps";
const localDate = (date: Date) => new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 10);
const localDateTime = (value: string) => {
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 19) : "";
};
const errorText = (error: unknown) => error instanceof Error ? error.message : "Could not load local memory. Please refresh.";
function bounds(start: string, end: string) {
  const from = new Date(`${start}T00:00:00`);
  const until = new Date(`${end}T00:00:00`);
  until.setDate(until.getDate() + 1);
  return { start_time: from.toISOString(), end_time: new Date(Math.min(until.getTime(), Date.now())).toISOString() };
}

export function MemoryExplorer() {
  const ui = useGT();
  const uiLocale = useUiLocale();
  const memoryErrorText = useMemoryErrorText();
  const number = (value: number) => value.toLocaleString(uiLocale);
  const time = (value: string) => new Date(value).toLocaleTimeString(uiLocale, { hour: "2-digit", minute: "2-digit" });
  const day = (value: string) => new Date(value).toLocaleDateString(uiLocale);
  const router = useRouter();
  const setPendingNavigation = useTimelineStore((state) => state.setPendingNavigation);
  const [tab, setTab] = useState<MemoryTab>("tree");
  const [start, setStart] = useState(() => localDate(new Date()));
  const [end, setEnd] = useState(() => localDate(new Date()));
  const [path, setPath] = useState<MemoryNode[]>([]);
  const [nodes, setNodes] = useState<MemoryNode[]>([]);
  const [detail, setDetail] = useState<MemoryNodeDetail | null>(null);
  const [summary, setSummary] = useState<HierarchySummary | null>(null);
  const [index, setIndex] = useState<MemoryIndexStatus | null>(null);
  const [marks, setMarks] = useState<MemoryMark[]>([]);
  const [markTotal, setMarkTotal] = useState(0);
  const [hits, setHits] = useState<MemorySearchHit[]>([]);
  const [query, setQuery] = useState("");
  const [offset, setOffset] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [editor, setEditor] = useState<MemoryMark | null>(null);
  const [configOpen, setConfigOpen] = useState(false);
  const [config, setConfig] = useState<SummaryConfig | null>(null);
  const [configLoading, setConfigLoading] = useState(false);
  const [configError, setConfigError] = useState<string | null>(null);
  const [summaryStatus, setSummaryStatus] = useState<SummaryStatus | null>(null);
  const [job, setJob] = useState<SummaryJob | null>(null);
  const [correction, setCorrection] = useState<"rename" | "merge" | "split" | null>(null);
  const [label, setLabel] = useState("");
  const [mergeId, setMergeId] = useState("");
  const [siblingQuery, setSiblingQuery] = useState("");
  const [splitAt, setSplitAt] = useState("");
  const [siblings, setSiblings] = useState<MemoryNode[]>([]);
  const [exported, setExported] = useState<MemoryExportManifest | null>(null);
  const selected = path.at(-1);
  const selectedId = selected?.id;
  const selectedNodeRef = useRef(selectedId);
  const activeTabRef = useRef(tab);
  selectedNodeRef.current = selectedId;
  activeTabRef.current = tab;
  const requestVersion = useRef(0);
  const privacyVersion = useRef(0);
  const loadedView = useRef("");
  const correctionRef = useRef(correction);
  correctionRef.current = correction;
  const clearProtected = useCallback(() => {
    privacyVersion.current++;
    setDetail(null); setSummary(null); setNodes([]); setHits([]); setMarks([]);
    setPath([]); setEditor(null); setSiblings([]); setSiblingQuery(""); setLabel(""); setCorrection(null); setJob(null);
    setExported(null);
  }, []);
  const accessFailure = (cause: unknown) => !!cause && typeof cause === "object" && "status" in cause && [401, 403, 503].includes(Number(cause.status));
  useEffect(() => { setJob(null); setSummary(null); }, [selectedId, tab]);
  useEffect(() => { setExported(null); }, [selectedId, tab, start, end]);
  useEffect(() => {
    const controller = new AbortController();
    api.summaryStatus(controller.signal).then((value) => { if (!controller.signal.aborted) setSummaryStatus(value); }).catch(() => {});
    return () => controller.abort();
  }, []);

  const openSource = useCallback((timestamp: string, frameId?: number | null) => {
    if (!Number.isFinite(Date.parse(timestamp))) return;
    setPendingNavigation({ timestamp, ...(frameId && Number.isSafeInteger(frameId) ? { frameId: String(frameId) } : {}) });
    router.push("/home?section=timeline");
  }, [router, setPendingNavigation]);
  const openMarkSource = (mark: MemoryMark) => openSource(mark.source_timestamp, mark.frame_id);
  const openNode = (node: MemoryNode) => {
    setPath((previous) => [...previous, node]); setOffset(0); setCorrection(null); setNotice(null);
  };
  const parameters = useCallback(() => {
    const range = bounds(start, end);
    return new URLSearchParams({ ...range, view: tab === "apps" ? "app" : "task", limit: "30", offset: String(offset), timezone_offset_minutes: String(-new Date().getTimezoneOffset()) });
  }, [start, end, offset, tab]);

  useEffect(() => {
    const controller = new AbortController();
    const version = ++requestVersion.current;
    const view = JSON.stringify([tab, selectedId, start, end, offset, query]);
    if (loadedView.current !== view) {
      loadedView.current = view;
      setLoading(true); setDetail(null); setSummary(null); setNodes([]); setHits([]); setMarks([]);
    }
    setError(null);
    const timer = setTimeout(async () => {
      try {
        if (start > end) throw new Error("The start date must come before the end date.");
        if (tab === "marks") {
          const page = await explicitMemoryApi.list(query, offset);
          if (version !== requestVersion.current || controller.signal.aborted) return;
          setMarks(page.data); setMarkTotal(page.pagination.total);
        } else if (tab === "search") {
          if (!query.trim()) {
            setHits([]);
            setIndex(await api.index(controller.signal));
            return;
          }
          const params = parameters(); if (query.trim()) params.set("q", query.trim());
          const page = await api.search(params, controller.signal);
          if (controller.signal.aborted) return;
          setHits(page.data); setIndex(page.index);
        } else {
          const params = parameters(); if (selectedId) params.set("parent_id", selectedId);
          const [page, current] = await Promise.all([
            api.tree(params, controller.signal),
            selectedId ? api.node(selectedId, controller.signal) : Promise.resolve(null),
          ]);
          if (controller.signal.aborted) return;
          setNodes(page.data); setIndex(page.index); setDetail(current);
          if (current) {
            if (!correctionRef.current) setLabel(current.node.label);
            const middle = (Date.parse(current.node.start_time) + Date.parse(current.node.end_time)) / 2;
            if (!correctionRef.current) setSplitAt(localDateTime(new Date(middle).toISOString()));
            const result = await api.summary(current.node.id, controller.signal);
            if (!controller.signal.aborted && result) {
              const value = "summary" in result ? result.summary : result as HierarchySummary;
              if (value && value.node_id !== current.node.id) throw new Error("The summary belongs to another activity. Refresh to try again.");
              setSummary(value);
            }
          }
        }
      } catch (cause) {
        if (!controller.signal.aborted && version === requestVersion.current) {
          clearProtected();
          setError(errorText(cause));
        }
      }
      finally { if (!controller.signal.aborted && version === requestVersion.current) setLoading(false); }
    }, query && !isTreeTab(tab) ? 250 : 0);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [tab, selectedId, parameters, query, offset, refresh, start, end, clearProtected]);

  useEffect(() => {
    if (!index) return;
    const controller = new AbortController();
    let lastSuccess = index.last_success_at;
    let privacyPending = index.privacy_pending;
    const timer = setInterval(() => {
      api.index(controller.signal).then((next) => {
        if (controller.signal.aborted) return;
        setIndex(next);
        if (next.privacy_pending) {
          privacyPending = true;
          clearProtected();
          setError("Privacy processing is in progress. Memory will reload when protected reads are available.");
          return;
        }
        if (privacyPending || next.rebuild_pending || next.last_success_at !== lastSuccess) {
          privacyPending = false;
          lastSuccess = next.last_success_at;
          setRefresh((value) => value + 1);
        }
      }).catch((cause) => {
        if (!controller.signal.aborted && accessFailure(cause)) {
          clearProtected(); setError(errorText(cause));
        }
      });
    }, 5000);
    return () => { controller.abort(); clearInterval(timer); };
  }, [Boolean(index), clearProtected]);
  useEffect(() => {
    if (!job?.job_id || job.node_id !== selectedId || !isTreeTab(tab) || !["pending", "running", "retrying"].includes(job.status)) return;
    const controller = new AbortController();
    const nodeId = selectedId;
    const timer = setInterval(() => {
      api.job(job.job_id!, controller.signal).then((next) => {
        if (controller.signal.aborted || selectedNodeRef.current !== nodeId || activeTabRef.current !== tab || next.node_id !== nodeId) return;
        setJob(next);
        if (next.status === "ready" || next.status === "completed") {
          if (next.summary?.node_id === nodeId) setSummary(next.summary);
          setRefresh((value) => value + 1);
          setNotice(ui("Summary saved locally."));
        } else if (next.status === "failed" || next.status === "stale") {
          setSummary(null); setRefresh((value) => value + 1);
          if (next.status === "failed") setError("The local model could not finish this summary. Check Local summaries and retry.");
          else setNotice(ui("Sources changed while generating. Refresh and generate again."));
        }
      }).catch((cause) => {
        if (!controller.signal.aborted && selectedNodeRef.current === nodeId && activeTabRef.current === tab) {
          clearProtected(); setJob(null); setError(errorText(cause));
        }
      });
    }, 2000);
    return () => { controller.abort(); clearInterval(timer); };
  }, [job?.job_id, job?.status, job?.node_id, selectedId, tab, clearProtected]);

  async function action(work: () => Promise<void>) {
    if (busy) return;
    setBusy(true); setError(null); setNotice(null);
    try { await work(); } catch (cause) { if (accessFailure(cause)) clearProtected(); setError(errorText(cause)); }
    finally { setBusy(false); }
  }
  async function loadConfig() {
    setConfigLoading(true); setConfigError(null);
    try {
      const [current, state] = await Promise.all([api.config(), api.summaryStatus()]);
      setConfig(current); setSummaryStatus(state);
    } catch (cause) { if (accessFailure(cause)) clearProtected(); setConfigError(errorText(cause)); }
    finally { setConfigLoading(false); }
  }
  function showConfig() {
    setConfigOpen((value) => !value);
    if (!configOpen) void loadConfig();
  }
  async function editCorrection(mode: "rename" | "merge" | "split") {
    setCorrection(mode); setMergeId(""); setSiblingQuery(""); setSiblings([]);
    if (mode !== "merge" || !detail) return;
    await findSiblings(detail.node, "");
  }
  async function findSiblings(target: MemoryNode, query: string) {
    await action(async () => {
      setMergeId(""); setSiblings([]);
      const params = parameters(); params.set("offset", "0"); params.set("limit", "50");
      params.set("kind", target.kind);
      if (target.parent_id) params.set("parent_id", target.parent_id);
      if (query.trim()) params.set("q", query.trim());
      const result = await api.tree(params);
      if (selectedNodeRef.current !== target.id || activeTabRef.current !== "tree" || correctionRef.current !== "merge") return;
      setSiblings(result.data.filter((node) => node.id !== target.id && node.kind === target.kind && node.parent_id === target.parent_id));
    });
  }
  async function applyCorrection() {
    if (!detail || !correction) return;
    await action(async () => {
      if (correction === "rename") await api.rename(detail.node, label);
      if (correction === "merge") await api.merge(detail.node, mergeId, label);
      if (correction === "split") await api.split(detail.node, new Date(splitAt).toISOString(), label);
      setCorrection(null); setPath((previous) => previous.slice(0, -1)); setOffset(0);
      setNotice(ui("Correction saved. Reindexing will preserve it.")); setRefresh((value) => value + 1);
    });
  }
  async function generateSummary(nodeId: string) {
    const targetTab = activeTabRef.current;
    await action(async () => {
      const result = await api.summarize(nodeId);
      if (selectedNodeRef.current !== nodeId || activeTabRef.current !== targetTab || !isTreeTab(targetTab)) return;
      if (result.node_id !== nodeId) throw new Error("The summary job belongs to another activity. Refresh to try again.");
      setJob(result);
      if (result.summary?.node_id === nodeId) setSummary(result.summary);
      setNotice(result.cached ? ui("The saved summary is current.") : ui("Summary queued on this computer."));
    });
  }
  const exportDate = selected ? localDate(new Date(selected.start_time)) : start === end ? start : null;
  async function exportSelectedDay() {
    if (!exportDate || !isTreeTab(tab)) return;
    const date = exportDate;
    const view = tab === "apps" ? "app" : "task";
    const nodeId = selectedId;
    const targetTab = tab;
    const version = privacyVersion.current;
    await action(async () => {
      setExported(null);
      let result: MemoryExportManifest;
      try { result = await api.exportDay(date, view); }
      catch (cause) {
        if (cause instanceof HierarchyApiError && cause.code === "memory_export_vault_enabled") {
          throw new Error(ui("Index exports are unavailable while encrypted storage is enabled."));
        }
        if (cause instanceof HierarchyApiError && cause.code === "memory_export_changed") {
          throw new Error(ui("The index or its sources changed during export. Please export again."));
        }
        throw cause;
      }
      if (privacyVersion.current !== version || selectedNodeRef.current !== nodeId || activeTabRef.current !== targetTab) return;
      if (result.date !== date || result.view !== view || typeof result.directory !== "string" || !result.directory) {
        throw new Error(ui("The export response did not match the selected day and view. Please retry."));
      }
      setExported(result);
    });
  }
  async function openCitation(citation: { source_type: string; source_id: string; timestamp: string }) {
    if (citation.source_type === "user_note") {
      await action(async () => { setEditor(await explicitMemoryApi.get(citation.source_id)); });
      return;
    }
    openSource(citation.timestamp, citation.source_type === "frame" ? Number(citation.source_id.replace(/^frame:/, "")) : undefined);
  }
  const stateLabels: Record<string, string> = { ready: ui("Ready"), pending: ui("Pending"), indexing: ui("Indexing"), partial: ui("Partial") };
  const availabilityLabels: Record<string, string> = { disabled: ui("Disabled"), no_model: ui("No model"), available: ui("Available"), unavailable: ui("Unavailable"), unknown: ui("Unknown") };
  const kindLabels: Record<string, string> = { day: ui("Day"), session: ui("Session"), task: ui("Task"), app: ui("Application"), location: ui("Location") };
  const locationLabels: Record<string, string> = { url: ui("Web page"), file: ui("File"), window: ui("Window"), unknown: ui("Unknown location") };
  const summaryStateLabels: Record<string, string> = { ready: ui("Ready"), pending: ui("Pending"), running: ui("Generating…"), retrying: ui("Retrying"), stale: ui("Stale"), failed: ui("Failed"), unavailable: ui("Unavailable") };
  const sourceLabels: Record<string, string> = { frame: ui("Screen"), audio: ui("Audio"), ui_event: ui("Interaction"), user_note: ui("Your note"), existing_memory: ui("Saved memory"), semantic_context: ui("Parsed source") };
  const status = index?.privacy_pending ? ui("Privacy processing") : index?.pending_jobs || index?.rebuild_pending ? ui("Indexing") : (index?.state ? stateLabels[index.state] ?? index.state : ui("Ready"));
  const jobBusy = !!job && ["pending", "running", "retrying"].includes(job.status);
  const renderMark = (mark: MemoryMark) => <article key={mark.id} className="space-y-2 rounded-lg border p-4">
    <p className="text-xs text-muted-foreground">Your note · {day(mark.source_timestamp)} {time(mark.source_timestamp)} · {mark.source_app || ui("Captured screen")}</p>
    <h3 className="font-medium break-words">{mark.title || ui("Untitled annotation")}</h3>
    {mark.note && <p className="whitespace-pre-wrap break-words text-sm">{mark.note}</p>}
    {mark.tags.length > 0 && <p className="text-xs text-muted-foreground">{mark.tags.join(" · ")}</p>}
    <p className="text-xs text-muted-foreground">{mark.retained_image ? ui("Source image retained") : ui("Original source")} · {number(mark.anchors.length)} selections</p>
    <div className="flex gap-2"><Button size="sm" variant="secondary" onClick={() => setEditor(mark)}>Open annotation</Button>
      {mark.frame_id !== null && <Button size="sm" variant="ghost" onClick={() => openMarkSource(mark)}>View source</Button>}</div>
  </article>;
  const renderEvidence = (evidence: MemoryEvidence) => <article key={`${evidence.source_type}:${evidence.source_id}`} className="border-l-2 pl-4 py-2 space-y-1">
    <p className="text-xs text-muted-foreground">{evidence.source_type === "user_note" ? ui("Your note") : ["memory", "existing_memory"].includes(evidence.source_type) ? ui("Saved memory") : ["semantic", "semantic_context"].includes(evidence.source_type) ? ui("Parsed source") : ui("Captured evidence")} · {time(evidence.timestamp)} · {evidence.app_name || sourceLabels[evidence.source_type] || evidence.source_type}</p>
    {evidence.excerpt && <p className="whitespace-pre-wrap break-words text-sm">{evidence.excerpt}</p>}
    {evidence.available ? <Button size="sm" variant="ghost" onClick={() => openSource(evidence.timestamp, evidence.frame_id)}>{evidence.frame_id ? ui("View source") : ui("View time in timeline")}</Button> : <p className="text-xs text-muted-foreground">Source unavailable</p>}
  </article>;

  return <section aria-label={ui("Memory explorer")} aria-busy={busy} className="mx-auto w-full max-w-5xl px-6 py-6"><fieldset disabled={busy} className="m-0 min-w-0 space-y-5 border-0 p-0">
    <header className="flex flex-wrap items-start justify-between gap-3">
      <div><h1 className="text-xl font-medium">Memory explorer</h1><p className="mt-1 text-sm text-muted-foreground">Browse tasks or applications, then follow their original sources.</p></div>
      <div className="flex gap-2"><Button size="sm" variant="outline" onClick={() => void showConfig()}><Settings2 className="mr-2 h-4 w-4" />Local summaries</Button>
        <Button aria-label={ui("Refresh memory")} size="icon" variant="ghost" onClick={() => setRefresh((value) => value + 1)}><RefreshCw className="h-4 w-4" /></Button></div>
    </header>
    <div className="flex flex-wrap items-center gap-3 border-y py-3">
      <div role="tablist" aria-label={ui("Memory views")} className="flex gap-1">{([['tree', ui('Task view')], ['apps', ui('App view')], ['marks', ui('My marks')], ['search', ui('Search memory')]] as const).map(([value, title]) =>
        <Button key={value} role="tab" aria-selected={tab === value} size="sm" variant={tab === value ? "secondary" : "ghost"} onClick={() => { setTab(value); setOffset(0); setPath([]); setQuery(""); setCorrection(null); }}>{title}</Button>)}</div>
      {index && <span className="ml-auto text-xs text-muted-foreground" role="status">{status} · {number(index.indexed_nodes)} nodes{index.failed_jobs ? ui(" · {count} failed jobs", { count: number(index.failed_jobs) }) : ""}</span>}
    </div>
    {configOpen && <div className="space-y-3 rounded-lg border p-4">
      <h2 className="font-medium">Local summaries</h2>
      <p className="text-sm text-muted-foreground">Summaries are generated on this computer from bounded source excerpts. Your notes remain separate from model interpretations.</p>
      {configLoading ? <p role="status" className="text-sm text-muted-foreground">Loading summary settings…</p> : configError ? <p role="alert" className="text-sm">{memoryErrorText(configError)}<Button size="sm" variant="ghost" onClick={() => void loadConfig()}>Retry settings</Button></p> : config ? <>
        <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={config.enabled} onChange={(event) => setConfig({ ...config, enabled: event.target.checked })} />Enable local summaries</label>
        <div className="grid gap-3 sm:grid-cols-2"><label className="grid gap-1 text-sm">Local Ollama URL<input aria-label={ui("Local Ollama URL")} className={field} value={config.base_url} onChange={(event) => setConfig({ ...config, base_url: event.target.value })} /></label>
          <label className="grid gap-1 text-sm">Model<input aria-label={ui("Summary model")} className={field} placeholder={ui("qwen3:1.7b")} value={config.model} onChange={(event) => setConfig({ ...config, model: event.target.value })} /></label></div>
        <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={config.background} onChange={(event) => setConfig({ ...config, background: event.target.checked })} />Summarize closed tasks in the background</label>
        <div className="flex items-center gap-3"><Button size="sm" disabled={busy || config.enabled && !config.model.trim()} onClick={() => void action(async () => { setConfig(await api.saveConfig(config)); setSummaryStatus(await api.summaryStatus()); setNotice(ui("Local summary settings saved.")); })}>Save summary settings</Button>
          <span className="text-xs text-muted-foreground">{summaryStatus?.availability ? availabilityLabels[summaryStatus.availability] ?? summaryStatus.availability : ""} · up to {number(config.jobs_per_hour)} jobs/hour</span></div>
      </> : <p className="text-sm text-muted-foreground">Loading summary settings…</p>}
    </div>}
    {tab !== "marks" && <div className="flex flex-wrap items-end gap-3">
      <label className="grid gap-1 text-xs text-muted-foreground">From<input aria-label={ui("Memory start date")} className={field} type="date" value={start} max={end} onChange={(event) => { if (event.target.value) { setStart(event.target.value); setPath([]); setOffset(0); } }} /></label>
      <label className="grid gap-1 text-xs text-muted-foreground">Through<input aria-label={ui("Memory end date")} className={field} type="date" value={end} min={start} max={localDate(new Date())} onChange={(event) => { if (event.target.value) { setEnd(event.target.value); setPath([]); setOffset(0); } }} /></label>
      <Button size="sm" variant="outline" disabled={busy || start > end || Date.parse(end) - Date.parse(start) > 30 * 86400000} onClick={() => void action(async () => { const range = bounds(start, end); await api.rebuild(range.start_time, range.end_time); setNotice(ui("Indexing queued for the selected dates. Recording continues.")); setRefresh((value) => value + 1); })}>Index selected dates</Button>
      {isTreeTab(tab) && <div className="space-y-1"><Button size="sm" variant="outline" disabled={busy || !exportDate || start > end} onClick={() => void exportSelectedDay()}>Export day</Button><p className="text-xs text-muted-foreground">{exportDate ? ui("Exports {date} as JSON and Markdown.", { date: day(`${exportDate}T12:00:00`) }) : ui("Open a day or select a single date to export.")}</p></div>}
    </div>}
    {exported && <div role="status" className="space-y-2 rounded-md border p-3 text-sm"><p>{ui("JSON and Markdown saved locally · {count} nodes", { count: number(exported.coverage.nodes_exported) })}</p>{exported.coverage.truncated && <p className="text-xs text-muted-foreground">Export coverage is bounded. Check each node's coverage for details.</p>}<Button size="sm" variant="secondary" onClick={() => void action(async () => { try { await revealItemInDir(exported.directory); } catch { throw new Error(ui("Could not open the export folder.")); } })}>Open export folder</Button></div>}
    {!isTreeTab(tab) && <label className="flex items-center gap-2"><Search className="h-4 w-4 text-muted-foreground" /><input aria-label={tab === "marks" ? ui("Search my marks") : ui("Search memory")} className={`${field} flex-1`} placeholder={tab === "marks" ? ui("Notes, selected text or tags") : ui("Search notes, tasks, summaries and source text")} value={query} onChange={(event) => { setQuery(event.target.value); setOffset(0); }} /></label>}
    {error && <div role="alert" className="rounded-md border p-3 text-sm">{memoryErrorText(error)}<Button size="sm" variant="ghost" onClick={() => setRefresh((value) => value + 1)}>Retry</Button></div>}
    {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
    {loading ? <p role="status" className="flex items-center gap-2 py-8 text-sm text-muted-foreground"><Loader2 className="h-4 w-4 animate-spin" />Loading local memory…</p> : <>
      {isTreeTab(tab) && <>
        {path.length > 0 && <nav aria-label={ui("Memory path")} className="flex flex-wrap items-center gap-1 text-sm"><Button size="sm" variant="ghost" onClick={() => { setPath([]); setOffset(0); setCorrection(null); }}><ArrowLeft className="mr-1 h-3 w-3" />Days</Button>{path.map((node, position) => <span key={node.id} className="flex items-center"><ChevronRight className="h-3 w-3" /><Button variant="ghost" size="sm" onClick={() => { setPath((previous) => previous.slice(0, position + 1)); setOffset(0); setCorrection(null); }}>{node.label}</Button></span>)}</nav>}
        {detail && <div className="space-y-4 rounded-lg border p-4">
          <div className="flex flex-wrap items-start justify-between gap-3"><div><p className="text-xs text-muted-foreground">{kindLabels[detail.node.kind] ?? detail.node.kind} · {detail.node.provisional ? ui("Recent activity can still change") : ui("Closed activity")}{detail.node.label_origin === "user" ? ui(" · Your label") : ui(" · Derived label")}</p>
            <h2 className="mt-1 text-lg font-medium break-words">{detail.node.label}</h2>{detail.node.location_type && <p className="mt-1 break-all text-xs text-muted-foreground">{locationLabels[detail.node.location_type] ?? detail.node.location_type}{detail.node.location_key && detail.node.location_key !== detail.node.label ? ` · ${detail.node.location_key}` : ""}</p>}<p className="mt-1 text-xs text-muted-foreground">{day(detail.node.start_time)} · {time(detail.node.start_time)}–{time(detail.node.end_time)} · {detail.node.apps.join(" · ")}</p></div>
            <div className="flex flex-wrap gap-1"><Button size="sm" variant="ghost" onClick={() => void editCorrection("rename")}>Rename</Button>{tab === "tree" && (detail.node.kind === "session" || detail.node.kind === "task") && <><Button size="sm" variant="ghost" onClick={() => void editCorrection("merge")}>Merge</Button>{detail.node.kind === "task" && <Button size="sm" variant="ghost" onClick={() => void editCorrection("split")}>Split</Button>}</>}</div></div>
          {correction && <div className="space-y-3 border-y py-3"><p className="text-sm">{correction === "rename" ? ui("Your label is preserved when the index refreshes.") : correction === "merge" ? ui("Combine sibling activity while preserving its source evidence.") : ui("Choose a boundary inside this activity. Original recordings stay intact.")}</p>
            <input aria-label={ui("Activity label")} className={`${field} w-full`} value={label} maxLength={200} onChange={(event) => setLabel(event.target.value)} />
            {correction === "merge" && <>
              <div className="flex flex-wrap gap-2"><input aria-label={ui("Find sibling activity")} className={`${field} flex-1`} placeholder={ui("Search sibling activity labels")} maxLength={200} value={siblingQuery} onChange={(event) => setSiblingQuery(event.target.value)} /><Button size="sm" variant="outline" onClick={() => void findSiblings(detail.node, siblingQuery)}>Search activities</Button></div>
              <select aria-label={ui("Activity to merge")} className={`${field} w-full`} value={mergeId} onChange={(event) => setMergeId(event.target.value)}><option value="">Choose a sibling activity</option>{siblings.map((node) => <option key={node.id} value={node.id}>{node.label} · {time(node.start_time)}</option>)}</select>
            </>}
            {correction === "split" && <input aria-label={ui("Split time")} type="datetime-local" step="1" className={field} min={localDateTime(detail.node.start_time)} max={localDateTime(detail.node.end_time)} value={splitAt} onChange={(event) => setSplitAt(event.target.value)} />}
            <div className="flex gap-2"><Button size="sm" disabled={busy || !label.trim() || correction === "merge" && !mergeId || correction === "split" && (!splitAt || Date.parse(splitAt) <= Date.parse(detail.node.start_time) || Date.parse(splitAt) >= Date.parse(detail.node.end_time))} onClick={() => void applyCorrection()}>Save correction</Button><Button size="sm" variant="ghost" onClick={() => setCorrection(null)}>Cancel</Button></div>
          </div>}
          <div className="space-y-2 border-t pt-3"><div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-medium">Generated summary</h3><Button size="sm" variant="outline" disabled={busy || jobBusy || detail.node.provisional || summaryStatus?.enabled === false || summaryStatus?.configured === false} onClick={() => void generateSummary(detail.node.id)}>{jobBusy ? ui("Generating…") : summary?.status === "ready" ? ui("Refresh summary") : ui("Generate summary")}</Button></div>
            {summary?.status === "ready" && summary.coverage && <p className="text-xs text-muted-foreground">Based on {number(summary.coverage.evidence_used)}/{number(summary.coverage.evidence_total)} source excerpts, {number(summary.coverage.marks_used)}/{number(summary.coverage.marks_total)} notes and {number(summary.coverage.children_used)}/{number(summary.coverage.children_total)} child summaries.{summary.coverage.truncated ? ui(" Bounded coverage; follow sources for details.") : ""}</p>}
            {summary?.text ? <><p className="text-xs text-muted-foreground">Model interpretation · {summaryStateLabels[summary.status] ?? summary.status} · {summary.model}{summary.generated_at ? ` · ${new Date(summary.generated_at).toLocaleString(uiLocale)}` : ""}</p><p className="whitespace-pre-wrap break-words text-sm">{summary.text}</p><div className="flex flex-wrap gap-1">{(summary.citations || []).map((citation, position) => <Button key={`${citation.source_type}:${citation.source_id}:${position}`} size="sm" variant="ghost" onClick={() => void openCitation(citation)}>{citation.source_type === "user_note" ? ui("Your note") : citation.source_type === "frame" ? ui("Source {number}", { number: number(position + 1) }) : ui("View time in timeline")} · {time(citation.timestamp)}</Button>)}</div></> : <p className="text-sm text-muted-foreground">{detail.node.provisional ? ui("Summaries become available after activity closes.") : summary?.status === "failed" ? ui("Summary generation failed. Check the local model and retry.") : summary?.status === "stale" ? ui("Sources or settings changed. Generate a fresh summary.") : ui("Enable a local model in Local summaries, then generate a source-cited summary.")}</p>}
          </div>
        </div>}
        <div className="grid gap-2">{nodes.map((node) => <button key={node.id} type="button" onClick={() => openNode(node)} className="flex items-center gap-3 rounded-lg border p-4 text-left hover:bg-muted/50 focus-visible:outline focus-visible:outline-2">
          <div className="min-w-0 flex-1"><p className="text-xs text-muted-foreground">{kindLabels[node.kind] ?? node.kind} · {node.kind === "day" ? day(node.start_time) : `${time(node.start_time)}–${time(node.end_time)}`}{node.provisional ? ui(" · Provisional") : ""}</p><p className="mt-1 break-words font-medium">{node.label}</p>{node.location_type && <p className="mt-1 break-all text-xs text-muted-foreground">{locationLabels[node.location_type] ?? node.location_type}{node.location_key && node.location_key !== node.label ? ` · ${node.location_key}` : ""}</p>}<p className="mt-1 text-xs text-muted-foreground">{number(node.evidence_count)} sources · {number(node.mark_count)} notes · {node.apps.join(" · ")}</p></div><ChevronRight className="h-4 w-4 shrink-0" /></button>)}</div>
        {!nodes.length && !detail && !error && <p className="py-10 text-center text-sm text-muted-foreground">No indexed activity in these dates. Choose Index selected dates to process saved recordings.</p>}
        {detail && <><div className="space-y-3"><h3 className="font-medium">Your notes · {number(detail.mark_total)}</h3>{detail.marks.map(renderMark)}{!detail.marks.length && <p className="text-sm text-muted-foreground">No annotations in this activity.</p>}</div><div className="space-y-3"><h3 className="font-medium">Source evidence · {number(detail.evidence_total)}</h3>{detail.evidence.map(renderEvidence)}{detail.truncated && <p className="text-xs text-muted-foreground">This is a bounded excerpt. Open a child activity or narrow the dates to inspect more sources.</p>}</div></>}
      </>}
      {tab === "marks" && <div className="grid gap-3">{marks.map(renderMark)}{!marks.length && !error && <p className="py-10 text-center text-sm text-muted-foreground">{query ? ui("No matching annotations.") : ui("No marks yet. Press Control + Command + M in another app to annotate a moment.")}</p>}</div>}
      {tab === "search" && <div className="grid gap-3">{hits.map((hit, position) => hit.mark ? renderMark(hit.mark) : hit.node ? <button key={`node:${hit.node.id}:${position}`} className="rounded-lg border p-4 text-left space-y-1 hover:bg-muted/50" onClick={() => { setTab(hit.node!.view === "app" || hit.node!.kind === "app" || hit.node!.kind === "location" ? "apps" : "tree"); setPath([hit.node!]); setOffset(0); }}><p className="text-xs text-muted-foreground">{hit.kind === "generated_summary" ? ui("Model interpretation") : ui("Derived activity")} · {kindLabels[hit.node.kind] ?? hit.node.kind}</p><p className="font-medium">{hit.node.label}</p><p className="text-xs text-muted-foreground">{day(hit.node.start_time)} · {number(hit.node.evidence_count)} sources</p></button> : hit.evidence ? <div key={`evidence:${position}`}>{renderEvidence(hit.evidence)}</div> : null)}{!hits.length && !error && <p className="py-10 text-center text-sm text-muted-foreground">{query.trim() ? ui("No matching memory in the selected dates.") : ui("Enter a phrase to search notes, tasks, summaries and source text.")}</p>}</div>}
      <div className="flex items-center justify-between border-t pt-3"><Button size="sm" variant="ghost" disabled={offset === 0} onClick={() => setOffset((value) => Math.max(0, value - (tab === "marks" ? 50 : 30)))}>Previous page</Button><span className="text-xs text-muted-foreground">{tab === "marks" ? ui("{count} annotations", { count: number(markTotal) }) : index?.coverage_start && index.coverage_end ? ui("Indexed {start} – {end}", { start: day(index.coverage_start), end: day(index.coverage_end) }) : ui("Source-backed local memory")}</span><Button size="sm" variant="ghost" disabled={tab === "marks" ? offset + marks.length >= markTotal : (isTreeTab(tab) ? nodes.length : hits.length) < 30} onClick={() => setOffset((value) => value + (tab === "marks" ? 50 : 30))}>Next page</Button></div>
    </>}
    </fieldset>{editor && <MarkEditor source={{ frameId: editor.source_frame_id, timestamp: editor.source_timestamp, app: editor.source_app || "" }} mark={editor} onClose={() => setEditor(null)} onSaved={() => { setEditor(null); setRefresh((value) => value + 1); }} />}
  </section>;
}
