// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { localFetch } from "@/lib/api";

export const WORKFLOWS_FLAG = "workflows";
export function isWorkflowsRolloutEnabled(flag: unknown): boolean {
  return flag === true;
}
export function useWorkflowsRolloutEnabled(): boolean {
  return true;
}
export function requireWorkflowsRollout(): void {
  // Local workflows do not require a hosted account rollout.
}

/** The engine starts closed; await this before dispatching a user-requested run. */
export async function syncWorkflowsRollout(enabled: boolean, signal?: AbortSignal): Promise<void> {
  const response = await localFetch("/workflows/rollout", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }), signal,
  });
  if (!response.ok) throw new Error("Could not confirm Workflows access. Try again.");
}
