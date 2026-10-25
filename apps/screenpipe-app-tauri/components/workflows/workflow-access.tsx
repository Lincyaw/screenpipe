// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { useSettings } from "@/lib/hooks/use-settings";
import { useHealthCheck } from "@/lib/hooks/use-health-check";
import { workflowAccess } from "@/lib/workflows/access";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { WorkflowTasksPrompt } from "./workflow-tasks-prompt";

export function WorkflowAccess({ active, requested = false, onRequestChange, onAccessChange }: { active: boolean; requested?: boolean; onRequestChange?: (open: boolean) => void; onAccessChange?: (reason: string | undefined) => void }) {
  const { settings } = useSettings();
  const { health, isServerDown } = useHealthCheck();
  const router = useRouter();
  const configured = settings.aiPresets.some(preset => preset.provider !== "screenpipe-cloud" && (preset.provider === "acp" ? !!preset.acpAgent?.id : !!preset.model));
  const reason = configured ? undefined : "Choose your AI model in Models & keys.";
  useEffect(() => { onAccessChange?.(reason); }, [onAccessChange, reason]);
  useEffect(() => { if (configured && requested) onRequestChange?.(false); }, [configured, requested, onRequestChange]);
  if (!active) return null;
  return <>
    <WorkflowTasksPrompt active backendReady={!!health && !isServerDown} canEnable={configured} onEnableUnavailable={() => onRequestChange?.(true)} />
    <Dialog open={requested && !configured} onOpenChange={onRequestChange}>
      <DialogContent className="max-w-sm">
        <DialogHeader><DialogTitle>Choose an AI model</DialogTitle><DialogDescription>Workflows use your own model and provider credentials.</DialogDescription></DialogHeader>
        <DialogFooter><Button onClick={() => { onRequestChange?.(false); router.push("/settings?section=ai"); }}>Models &amp; keys</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </>;
}

// Retained for callers compiled against the shared workflow notice contract.
export function WorkflowAccessNotice({ open = false, onOpenChange, onAccount }: {
  open?: boolean; onOpenChange?: (open: boolean) => void;
  access: ReturnType<typeof workflowAccess>; refreshing?: boolean; onRetry: () => void; onAccount: () => void; onUsage: () => void;
}) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent><DialogHeader><DialogTitle>Choose an AI model</DialogTitle><DialogDescription>Configure your own provider in Models &amp; keys.</DialogDescription></DialogHeader></DialogContent></Dialog>;
}
