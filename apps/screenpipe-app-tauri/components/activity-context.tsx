// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useState } from "react";
import { ActivityLedger } from "@/components/activity-ledger";
import { MemoryExplorer } from "@/components/memory-explorer";
import { Button } from "@/components/ui/button";

export function ActivityContext({ onOpenArtifact }: { onOpenArtifact?: () => void }) {
  const [memoryOpen, setMemoryOpen] = useState(false);
  return <div className="flex h-full min-h-0 flex-col">
    <nav aria-label="Activity and memory" className="flex shrink-0 gap-1 border-b px-6 py-2">
      <Button size="sm" variant={memoryOpen ? "ghost" : "secondary"} aria-pressed={!memoryOpen} onClick={() => setMemoryOpen(false)}>Recent activity</Button>
      <Button size="sm" variant={memoryOpen ? "secondary" : "ghost"} aria-pressed={memoryOpen} onClick={() => setMemoryOpen(true)}>Memory explorer</Button>
    </nav>
    <div className="min-h-0 flex-1 overflow-y-auto">{memoryOpen ? <MemoryExplorer /> : <ActivityLedger onOpenArtifact={onOpenArtifact} />}</div>
  </div>;
}
