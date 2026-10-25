// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import { localFetch } from "@/lib/api";
import type { MemoryMark } from "@/lib/explicit-memory";

export type MemoryCitation = { source_type: string; source_id: string; timestamp: string; uri?: string };
export type HierarchySummary = {
  node_id: string; node_fingerprint: string; node_revision: number; error_code?: string | null;
  status: string; text: string | null; model: string; provider: string; prompt_version: string;
  input_fingerprint: string; generated_at: string | null; citations: MemoryCitation[];
  user_notes_attributed: boolean;
  coverage?: { evidence_used: number; evidence_total: number; marks_used: number; marks_total: number; children_used: number; children_total: number; truncated: boolean };
};
export type MemoryNode = {
  id: string; parent_id: string | null; kind: "day" | "session" | "task";
  label: string; label_origin: string; start_time: string; end_time: string;
  provisional: boolean; evidence_count: number; mark_count: number; apps: string[];
  revision: number; fingerprint: string; children?: MemoryNode[]; summary?: HierarchySummary | null;
};
export type MemoryEvidence = Omit<MemoryCitation, "source_id"> & {
  source_id: string | number; app_name?: string | null; excerpt: string | null; frame_id?: number | null;
  mark_id?: string | null; available: boolean; window_title?: string | null;
};
export type MemoryNodeDetail = {
  node: MemoryNode; evidence: MemoryEvidence[]; marks: MemoryMark[]; children: MemoryNode[];
  evidence_total: number; mark_total: number; children_total: number; truncated: boolean;
};
export type MemoryIndexStatus = {
  generation: number; state: string; pending_jobs: number; failed_jobs: number;
  cursor: string | null; last_success_at: string | null; last_error: string | null;
  indexed_nodes: number; provisional_nodes: number; coverage_start: string | null;
  coverage_end: string | null; rebuild_pending: boolean; privacy_pending: boolean; timezone_offset_minutes: number;
};
export type MemoryTreePage = { data: MemoryNode[]; index: MemoryIndexStatus };
export type MemorySearchHit = {
  kind: string; node?: MemoryNode | null; mark?: MemoryMark | null;
  evidence?: MemoryEvidence | null; score: number;
};
export type SummaryConfig = {
  enabled: boolean; base_url: string; model: string; background: boolean;
  max_input_chars: number; max_evidence: number; max_children: number;
  max_output_tokens: number; request_timeout_secs: number; jobs_per_hour: number;
};
export type SummaryJob = {
  job_id: string | null; node_id: string; status: string; attempts?: number;
  error_code?: string | null; summary?: HierarchySummary | null; cached?: boolean;
};
export type SummaryStatus = {
  enabled: boolean; configured: boolean; provider: string; availability: string;
  pending_jobs: number; running_jobs: number; failed_jobs: number; last_error_code?: string | null;
};

export class HierarchyApiError extends Error {
  constructor(public status: number, public code?: string) {
    super(code === "summary_disabled" ? "Enable local summaries and select a model in Local summaries."
      : code === "no_model" || code === "summary_model_required" || code === "summary_not_configured" ? "Choose an installed local model in Local summaries."
      : code === "node_not_closed" ? "This activity is still changing. Choose a closed task or session."
      : code === "local_only_not_verified" ? "Ollama must run with cloud features disabled before summarizing private evidence."
      : status === 409 ? "This item changed. Refresh it before applying your correction."
      : status === 401 || status === 403 ? "Local owner access is required."
      : status === 404 ? "This item or its source is no longer available."
      : status === 400 ? "Check the selected dates, label and model settings."
      : status === 429 ? "The processing budget is in use. Try again later."
      : status === 503 ? "This data is not ready. Wait for indexing or privacy processing, then refresh."
      : "Could not complete this request. Please try again.");
  }
}
async function request<T>(path: string, method = "GET", body?: unknown, signal?: AbortSignal): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    signal?.throwIfAborted();
    const response = await localFetch(path, {
      method, signal, cache: "no-store",
      ...(body === undefined ? {} : { headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }),
    });
    if (response.ok) {
      const data = await response.json();
      signal?.throwIfAborted();
      return data;
    }
    const data = await response.json().catch(() => ({}));
    signal?.throwIfAborted();
    const code = data && typeof data.code === "string" ? data.code : undefined;
    // A concurrent local index update can invalidate a protected read. Only
    // retry those GET conflicts, using a fresh request and backend admission.
    if (method === "GET" && attempt < 3 && response.status === 409 &&
      (code === "memory_source_changed" || code === "memory_revision_conflict")) {
      await new Promise<void>((resolve, reject) => {
        const onAbort = () => {
          clearTimeout(timer);
          signal?.removeEventListener("abort", onAbort);
          reject(signal?.reason ?? new DOMException("The operation was aborted.", "AbortError"));
        };
        const timer = setTimeout(() => {
          signal?.removeEventListener("abort", onAbort);
          resolve();
        }, 150);
        signal?.addEventListener("abort", onAbort, { once: true });
        if (signal?.aborted) onAbort();
      });
      continue;
    }
    throw new HierarchyApiError(response.status, code);
  }
}
const nodePath = (id: string) => `/memory/nodes/${encodeURIComponent(id)}`;
export const hierarchicalMemoryApi = {
  tree(params: URLSearchParams, signal?: AbortSignal) { return request<MemoryTreePage>(`/memory/tree?${params}`, "GET", undefined, signal); },
  node(id: string, signal?: AbortSignal) { return request<MemoryNodeDetail>(nodePath(id), "GET", undefined, signal); },
  index(signal?: AbortSignal) { return request<MemoryIndexStatus>("/memory/index/status", "GET", undefined, signal); },
  search(params: URLSearchParams, signal?: AbortSignal) { return request<{ data: MemorySearchHit[]; index: MemoryIndexStatus }>(`/memory/search?${params}`, "GET", undefined, signal); },
  rebuild(start_time: string, end_time: string) { return request<{ jobs: unknown[]; index: MemoryIndexStatus }>("/memory/index/rebuild", "POST", { start_time, end_time, timezone_offset_minutes: -new Date().getTimezoneOffset() }); },
  rename(node: MemoryNode, label: string) { return request<{ node: MemoryNode; index: MemoryIndexStatus }>(nodePath(node.id), "PATCH", { revision: node.revision, label }); },
  merge(node: MemoryNode, other: string, label: string) { return request<{ node: MemoryNode; index: MemoryIndexStatus }>(`${nodePath(node.id)}/merge`, "POST", { revision: node.revision, node_ids: [other], ...(label.trim() ? { label } : {}) }); },
  split(node: MemoryNode, at: string, label: string) { return request<{ node: MemoryNode; index: MemoryIndexStatus }>(`${nodePath(node.id)}/split`, "POST", { revision: node.revision, at, ...(label.trim() ? { label } : {}) }); },
  config(signal?: AbortSignal) { return request<SummaryConfig>("/memory/summary/config", "GET", undefined, signal); },
  saveConfig(config: SummaryConfig) { return request<SummaryConfig>("/memory/summary/config", "PUT", config); },
  summaryStatus(signal?: AbortSignal) { return request<SummaryStatus>("/memory/summary/status", "GET", undefined, signal); },
  summary(id: string, signal?: AbortSignal) { return request<HierarchySummary | { summary: HierarchySummary | null }>(`${nodePath(id)}/summary`, "GET", undefined, signal); },
  summarize(id: string) { return request<SummaryJob>(`${nodePath(id)}/summarize`, "POST", {}); },
  job(id: string, signal?: AbortSignal) { return request<SummaryJob>(`/memory/summary/jobs/${encodeURIComponent(id)}`, "GET", undefined, signal); },
};
