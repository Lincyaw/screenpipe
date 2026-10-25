// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { Receipts } from "./receipts";
import type { ActivitySummary } from "@/lib/utils/meeting-context";
afterEach(cleanup);
it("shows app receipts as source data without launching chat", () => {
  const activity: ActivitySummary = {
    apps: [], windows: [{ app_name: "Arc", window_name: "Customer onboarding notes", browser_url: "", minutes: 13.3, frame_count: 42 }],
    edited_files: [], audio_summary: { segment_count: 0, speakers: [], top_transcriptions: [] }, total_frames: 42,
    time_range: { start: "2026-08-24T17:00:00.000Z", end: "2026-08-24T17:30:00.000Z" },
  };
  render(<Receipts activity={activity} />);
  expect(screen.getByText("Arc")).toBeVisible();
  expect(screen.getByText("Customer onboarding notes")).toBeVisible();
  expect(screen.getByText("13.3m")).toBeVisible();
  expect(screen.queryByRole("button", { name: /ask|chat/i })).toBeNull();
});
