<!-- screenpipe — AI that knows everything you've seen, said, or heard -->
<!-- https://screenpipe.com -->

# Local annotations and activity memory

<!-- doc-covers: apps/screenpipe-app-tauri/components/explicit-memory, apps/screenpipe-app-tauri/app/annotate, apps/screenpipe-app-tauri/lib/explicit-memory.ts, apps/screenpipe-app-tauri/src-tauri/src/annotation_capture.rs, crates/screenpipe-db/src/db/explicit_memory.rs, crates/screenpipe-db/src/db/marked_capture.rs, crates/screenpipe-engine/src/routes/explicit_memory.rs, crates/screenpipe-engine/src/marked_capture.rs, apps/screenpipe-app-tauri/components/memory-explorer.tsx, apps/screenpipe-app-tauri/lib/hierarchical-memory.ts, crates/screenpipe-db/src/db/hierarchical_memory.rs, crates/screenpipe-db/src/db/hierarchical_memory, crates/screenpipe-db/src/db/memory_exports.rs, crates/screenpipe-db/src/migrations/20261026010000_incremental_memory_projection.sql, crates/screenpipe-db/src/migrations/20261026020000_memory_mark_projection_recount.sql, crates/screenpipe-db/src/db/hierarchy_summary.rs, crates/screenpipe-engine/src/hierarchical_memory.rs, crates/screenpipe-engine/src/hierarchy_summary.rs, crates/screenpipe-engine/src/routes/hierarchical_memory.rs, crates/screenpipe-engine/src/routes/memory_exports.rs, apps/screenpipe-app-tauri/src-tauri/swift/timeline/TimelineViewModel.swift, apps/screenpipe-app-tauri/src-tauri/swift/timeline/TimelineWindow.swift, packages/screenpipe-mcp/src/memory-tools.ts -->
<!-- doc-verified: 5d84243ab130270c951543846f87fdec367cf19a -->

This personal-fork feature saves user notes, regions, and text quotes against exact local frames. Capture a new screenshot from another application, or annotate a historical frame in Timeline. A saved annotation can be searched, edited, opened at its source, and deleted without changing the original screenshot.

## Capture from another application

On macOS, press **Control + Command + M** while working in another application. The shortcut captures the display under the pointer through the existing recording worker. The annotation window appears only after the new screenshot has been durably saved and released under the current privacy settings. It never substitutes a previous frame.

Drag on the frozen screenshot, enter a note, and choose **Save annotation**. **Command + Enter** also saves. New captures keep the source image by default; the editor displays this choice. Saving or pressing **Esc** to cancel closes the editor and returns focus to the previous application. Cancelling creates no annotation. Pressing the shortcut while a draft is open returns to that draft.

Change or disable **Capture and annotate** in **Settings → Shortcuts**. Paused recording, disabled screenshots, excluded targets, missing permissions, and protected content block capture. The shortcut does not resume recording or bypass privacy controls. Fresh shortcut capture currently requires macOS; historical annotations remain available on other platforms.

Choose **Capture and annotate** from the menu bar icon to use the same capture flow with the mouse. The menu closes before capture begins.

## Save and find an annotation

Open Timeline and select **Enable annotations**, then **Annotate**. The controls default off and become enabled after the first successful shortcut annotation; the opt-in is stored locally under `screenpipe:explicit-memory:enabled`. Disabled controls do not fetch annotation data.

Drag on the frozen source image to add a region. Add a title or note, optional text quotes, comma-separated tags, and importance. Quotes are user-selected text; the editor does not claim a stable OCR or accessibility element link. Choose whether to keep the source image, then save. **My marks** searches notes, quotes, selection comments, and tags. **This frame only** limits the list to the selected source.

**Open annotation** edits the saved note and selections. **View in timeline** navigates to the original local frame. Missing sources are reported; image reads explicitly disable substitution with a nearby frame. Editing uses a revision check, so a stale editor reports a conflict and offers to reload the saved version.

**Delete annotation** removes its note, anchors, tag links, search document, and retained image. It leaves the original ambient recording intact. **Disable annotations** hides the feature without deleting saved annotations.

## Retention and privacy

Retained images are bounded BLOB assets in the existing database storage boundary, with an 8 MiB encoded-image limit. All annotation writes use the existing database write coordinator. Image preparation runs before the write transaction, with bounded concurrency held through persistence.

Ordinary retention may remove the raw frame while preserving an explicitly retained image. Without retention, the note remains but its source image can become unavailable. Original frame ID, timestamp, and available image dimensions remain on the annotation.

Explicit source deletion through the existing `/data/delete` time-range path erases matching annotations and retained images by their immutable source timestamp, including when the raw frame has already expired. Copied evidence is bound to the privacy revocation state at save time. Trusted ordinary-retention transactions preserve the validity of currently released saved evidence without reviving stale marks. Other global revocations conservatively withhold previously saved marks and images, including unrelated marks; authored text remains stored. This increment has no automatic evidence revalidation worker. A `privacy_revalidation_required` response does not promise automatic recovery after ordinary processing finishes.

Saving, editing, and keyword search use no model or cloud service. Annotations have separate tables and are not enrolled in existing memory sync. Stored source metadata and note bodies are not included in API error responses.

## Use the local API

All annotation endpoints currently require the configured local owner's API key in the Bearer header. Pipe tokens, cookies alone, and permissive pipe endpoint rules do not grant access. This increment adds no agent write tools.

| Method | Path | Operation |
| --- | --- | --- |
| `POST` | `/capture/marked` | Request a fresh capture with UUID `request_id` and optional `monitor_id`; return `202` with request state |
| `GET` | `/capture/marked/:id` | Read `pending`, `completed`, `blocked`, `failed`, or `interrupted`; completion includes the exact committed frame and image geometry |
| `POST` | `/memory/marks` | Create a mark with a client UUID and exact `frame_id` |
| `GET` | `/memory/marks` | List or search using `q`, `frame_id`, `tag`, `limit`, and `offset` |
| `GET` | `/memory/marks/:id` | Read one mark |
| `PATCH` | `/memory/marks/:id` | Update fields with the current `revision` |
| `DELETE` | `/memory/marks/:id?revision=N` | Delete with the current revision |
| `GET` | `/memory/marks/:id/image` | Read retained pixels or the exact available source image |

The request and response types live in [the desktop client](../apps/screenpipe-app-tauri/lib/explicit-memory.ts); input limits and authorization live in [the engine route](../crates/screenpipe-engine/src/routes/explicit_memory.rs). Duplicate creation IDs with matching initial content replay the saved result. Conflicting content or stale update/delete revisions return `409`. Images use `Cache-Control: no-store`.

Capture requests use a bounded durable journal. Replaying a UUID with the same display selector returns its existing result. A different selector returns `409`. Pending requests expire after 30 seconds and become interrupted across restart. Terminal receipts expire after 24 hours; saved annotations have their own retention lifecycle. Reading a completed receipt rechecks current access to the exact source image.

## Verify changes

From `apps/screenpipe-app-tauri`:

```sh
bun run test:vitest lib/explicit-memory.test.ts components/explicit-memory
bun run typecheck
bun run dev:web
```

Open `http://127.0.0.1:1420/dev/explicit-memory` for the fictional preview. It uses an in-memory fixture and is unavailable outside browser-mock mode. Browser checks exercise layout and interactions; they do not establish native permissions or live database behavior.

From the repository root:

```sh
cargo test -p screenpipe-db --lib explicit_memory
cargo test -p screenpipe-engine --lib routes::explicit_memory::tests
```

The tests cover transactional search updates, Chinese substring matches, conflicts, idempotency, original geometry, retained images across retention and restart, explicit erasure, privacy changes, and continued frame writes after annotation failure. Existing regression checks for shared tag suggestions, image privacy, and retention are:

```sh
cargo test -p screenpipe-db --test tag_autocomplete_query_test --test snapshot_compaction_test
cargo test -p screenpipe-db --lib wal_maintenance_tests::retention
```

## Scope

The local memory layer now includes historical annotations, fresh shortcut captures on macOS, a persisted activity hierarchy, optional local summaries, and read-only MCP retrieval. Cross-device sharing and arbitrary interval annotations remain outside this implementation. Existing capture and processing policies still apply.

## Browse the activity hierarchy

Open **Activity → Memory explorer** and choose a view:

- **Tasks**: day → session → task → exact source evidence. Related work in a browser and editor can share a task.
- **Applications**: day → application → webpage, file or window → exact source evidence. A URL takes precedence over a captured document path, then the window title; an unknown location stays explicitly unknown.

Both views use the same node, source-reference, annotation, summary and privacy mechanisms. They do not copy recordings or run separate capture/search pipelines. Existing task IDs and authored corrections remain usable. Application and location labels can be renamed; merge and split remain task-view corrections. Labels and boundaries are deterministic inferences; they do not establish the user's actual intent.

Choose dates and **Index selected dates** for bounded historical backfill, up to 31 days per request. The initial worker prioritizes the most recent hour and backfills seven days. Durable jobs and source cursors resume after restart. A sliding reconciliation window accommodates late transcription. Projection updates are paged, idempotent batches of source references and affected ancestors; daily evidence counts do not impose a recording or indexing ceiling. The processing budget bounds each background batch, rather than requiring an entire day to fit in one transaction. Reads use the saved index rather than rescanning the complete capture history. Processing failures defer index work while capture continues through the existing writer.

Open a node to inspect child activities, annotations, source counts, and bounded evidence excerpts. **View source** opens an exact frame. Evidence without a frame offers **View time in timeline**, which navigates to that timestamp rather than claiming an exact screenshot match. **My marks** searches saved annotations independently of the tree.

**Rename** saves a user label. **Merge** combines sibling sessions or tasks. **Split** divides a task at a selected time inside its interval. Corrections are separate persisted overrides, survive reindexing, and use optimistic revisions. Rebuilds do not modify original recordings or mark text. Changes invalidate affected summaries and ancestors.

**Search memory** merges authored annotations, context nodes, ready summaries, raw frame/audio search, existing memory records, and semantic records. Results retain their source types; an annotation's relevance score is not factual confidence. Candidate windows and result sizes are bounded, so coverage/truncation metadata must be preserved when an agent uses a result. Missing or withheld sources are not silently replaced.

## Generate local hierarchical summaries

In **Memory explorer → Local summaries**, enable the feature, enter a local Ollama URL and an installed model, and save. Summaries are off by default. Background generation is a separate opt-in; on-demand generation works without enabling it. A typical local runtime setup is:

```sh
brew install ollama
OLLAMA_NO_CLOUD=1 ollama serve
ollama pull qwen3:1.7b
```

The runtime must explicitly report that cloud functionality is disabled and that the selected model is local. The worker accepts only a loopback endpoint, disables redirects and proxies, and rejects remote/cloud model metadata before sending source excerpts. A reachable local endpoint alone is not treated as proof of local inference. For a persistent service, save `{"disable_ollama_cloud":true}` in Ollama's `~/.ollama/server.json` and use the runtime's normal service manager. Preserve other existing runtime settings.

Choose a closed task, session or day and **Generate summary**. Child summaries are generated before their parent. Each result records model/provider, prompt version, input fingerprint, generation time, typed statements, source citations and bounded input coverage. User-authored notes and corrections are explicitly attributed; model output cannot change them. Read a citation before treating a model interpretation as evidence.

Jobs are persisted, budgeted and retried with backoff. Changed inputs invalidate cached results. Failed, stale and unavailable results expose status without old summary text. With no configured or reachable model, indexing, tree browsing and keyword retrieval remain available. Quitting into saved-history mode suspends indexing and inference while allowing authenticated reads.

## Export a daily index

Open a day or one of its descendants and choose **Export day**, or select a single date before exporting. One explicit owner request produces JSON and Markdown from the same versioned snapshot under the active data directory's `memory/exports/<date>/<export-id>/`. The files contain readable labels, ready summaries, source links and coverage metadata; they do not copy raw recording excerpts or screenshots. A bounded export can be partial: check its `coverage` and `truncated` fields before treating it as a complete daily history.

These are managed snapshots, not the canonical index. Recording continues independently. Source erasure, retention deletion, privacy-policy changes, and relevant annotation changes revoke managed snapshots before their mutation commits. Copies taken outside the managed directory are independent user exports. Vault-enabled profiles reject plaintext file export. MCP reads bounded manifests through the authenticated API; it cannot choose a filesystem path, create an export or read arbitrary files.

## Read through MCP

The repository's MCP build exposes the same memory tools over stdio and HTTP:

| Tool | Purpose |
| --- | --- |
| `search-marks` | Find user-authored annotations |
| `read-mark` | Read one annotation, quotes and normalized regions |
| `get-mark-image` | Read image metadata; include pixels only when explicitly requested |
| `memory-tree` | Expand task or application views; omit `view` to retain the task view |
| `memory-node` | Read one node's sources, annotations and children |
| `search-memory` | Search across authored, captured and derived context |
| `memory-index-status` | Inspect persisted index progress and coverage |
| `read-memory-summary` | Read an existing source-cited summary |
| `memory-summary-status` | Inspect local model availability and queued work |
| `memory-exports` | List managed daily index snapshots |
| `read-memory-export` | Read a bounded snapshot manifest and its coverage |

These MCP tools use the configured local owner's API key. They cannot start capture, write annotations, correct the tree, schedule a rebuild, generate summaries or create exports. The desktop owner controls those memory operations through the UI/API. Other existing MCP tools keep their separate capabilities. Memory tools enforce query/output limits, preserve source identifiers and attribution, and withhold stale summary text. Build the local package with `cd packages/screenpipe-mcp && bun run build`; a registry package does not include unshipped repository changes. Debug desktop builds locate this checkout’s existing `dist/cli.js` and configure detected clients to use it, including when using the production data profile. Explicit `SCREENPIPE_MCP_ENTRYPOINT` and `SCREENPIPE_MCP_BUN` overrides must be existing absolute local paths; invalid overrides fail without replacing client configuration or downloading a fallback. Restart the Screenpipe MCP connection in the agent after rebuilding to refresh its tool inventory.

`GET /memory/tree?view=task` retains the existing hierarchy; `view=app` exposes application and location nodes using the same response shape and opaque IDs. `POST /memory/exports` accepts one `{date, view, timezone_offset_minutes}`. `GET /memory/exports` lists manifests, and `GET /memory/exports/:id/manifest` pages through an existing snapshot. Export paths are returned only to the owner creating the files.

The additional owner-authenticated API routes are `GET /memory/tree`, `GET /memory/nodes/:id`, `GET /memory/search`, `GET /memory/index/status`, `POST /memory/index/rebuild`, `PATCH /memory/nodes/:id`, `POST /memory/nodes/:id/merge`, `POST /memory/nodes/:id/split`, `GET/PUT /memory/summary/config`, `POST /memory/nodes/:id/summarize`, `GET /memory/nodes/:id/summary`, `GET /memory/summary/jobs/:id`, and `GET /memory/summary/status`. Background workers and all derived writes use the existing database coordinator; inference and source hydration happen outside write transactions.
