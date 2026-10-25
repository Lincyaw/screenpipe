// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com
"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { LanguageSelector } from "@/components/language-selector";
import { Button } from "@/components/ui/button";
import PermissionsStep from "@/components/onboarding/permissions-step";
import EngineStartup from "@/components/onboarding/engine-startup";
import { useOnboardingWithLoader } from "@/lib/hooks/use-onboarding";
import { useSettings } from "@/lib/hooks/use-settings";
import { useLocalizationEnabled } from "@/lib/i18n/provider";
import { commands } from "@/lib/utils/tauri";

type Step = "capture" | "permissions" | "engine" | "finish";
const STEPS: Step[] = ["capture", "permissions", "engine", "finish"];

export default function OnboardingPage() {
  const localizationEnabled = useLocalizationEnabled();
  const { onboardingData, isLoading, completeOnboarding } = useOnboardingWithLoader();
  const { settings, isSettingsLoaded, updateSettings } = useSettings();
  const [step, setStep] = useState<Step>("capture");
  const [screen, setScreen] = useState(false);
  const [audio, setAudio] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const initialized = useRef(false);
  const advancing = useRef(false);

  useEffect(() => {
    void commands.setWindowSize("Onboarding", 500, 680).catch(() => {});
  }, []);

  useEffect(() => {
    if (isLoading || !isSettingsLoaded || initialized.current) return;
    initialized.current = true;
    setScreen(!settings.disableVision);
    setAudio(!settings.disableAudio);
    const saved = onboardingData.currentStep;
    // Old account, checkout and experiment steps restart at capture choices.
    setStep(saved === "finish" ? "engine" : STEPS.includes(saved as Step) ? saved as Step : "capture");
  }, [isLoading, isSettingsLoaded, onboardingData.currentStep, settings.disableVision, settings.disableAudio]);

  useEffect(() => {
    if (!onboardingData.isCompleted) return;
    void commands.showWindow({ Home: { page: "timeline" } })
      .then(() => window.close()).catch(() => {});
  }, [onboardingData.isCompleted]);

  const advance = useCallback(async (next: Step) => {
    if (advancing.current) return;
    advancing.current = true;
    setBusy(true);
    setError(null);
    try {
      if (step === "capture") {
        await updateSettings({
          disableVision: !screen,
          disableAudio: !audio,
          disableScreenshots: !screen,
          audioCaptureMode: "always",
        });
      }
      const result = await commands.setOnboardingStep(next);
      if (result?.status === "error") throw new Error(result.error);
      setStep(next);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not save your setup. Please try again.");
    } finally {
      advancing.current = false;
      setBusy(false);
    }
  }, [audio, screen, step, updateSettings]);

  const finish = async () => {
    if (advancing.current) return;
    advancing.current = true;
    setBusy(true);
    setError(null);
    try {
      await completeOnboarding({ method: "setup_finished" });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not finish setup. Please try again.");
    } finally {
      advancing.current = false;
      setBusy(false);
    }
  };

  if ((isLoading && !initialized.current) || !isSettingsLoaded) {
    return <div role="status" className="flex min-h-screen items-center justify-center bg-background">Loading setup…</div>;
  }

  return (
    <div className="flex h-screen w-full flex-col overflow-hidden bg-background text-foreground">
      <div className="p-3" data-tauri-drag-region />
      {localizationEnabled && <div className="px-6"><LanguageSelector /></div>}
      <main className="min-h-0 flex-1 overflow-y-auto p-6">
        <div className="mx-auto flex min-h-full max-w-lg flex-col justify-center gap-5">
          <p className="text-xs text-muted-foreground">Setup · {STEPS.indexOf(step) + 1} of {STEPS.length}</p>
          {step === "capture" && <>
            <h1 className="text-2xl font-semibold">Choose what to record</h1>
            <p className="text-sm text-muted-foreground">Your history is stored on this computer. You can pause recording or change these choices in Settings.</p>
            <label className="flex items-start gap-3 rounded-lg border border-border p-4">
              <input type="checkbox" checked={screen} onChange={(event) => setScreen(event.target.checked)} disabled={busy} />
              <span><strong className="block text-sm">Screen history</strong><span className="text-sm text-muted-foreground">Save screenshots and searchable app and window text.</span></span>
            </label>
            <label className="flex items-start gap-3 rounded-lg border border-border p-4">
              <input type="checkbox" checked={audio} onChange={(event) => setAudio(event.target.checked)} disabled={busy} />
              <span><strong className="block text-sm">Microphone and system audio</strong><span className="text-sm text-muted-foreground">Record continuously while Screenpipe runs, with local transcription.</span></span>
            </label>
            <Button disabled={busy} onClick={() => void advance("permissions")}>{busy ? "Saving…" : "Continue"}</Button>
          </>}
          {step === "permissions" && <PermissionsStep handleNextSlide={() => void advance("engine")} />}
          {step === "engine" && <EngineStartup handleNextSlide={() => void advance("finish")} />}
          {step === "finish" && <>
            <h1 className="text-2xl font-semibold">Ready to use Screenpipe</h1>
            <p className="text-sm text-muted-foreground">Open Timeline to browse your history by date. New recordings appear as you work.</p>
            <p className="text-sm text-muted-foreground">AI features use a model you choose in Settings → Models &amp; keys.</p>
            <Button disabled={busy} onClick={() => void finish()}>{busy ? "Opening…" : "Open timeline"}</Button>
          </>}
          {error && <div role="alert" className="rounded-md border border-border p-3 text-sm">{error}{step !== "capture" && step !== "finish" && <Button variant="ghost" disabled={busy} onClick={() => void advance(STEPS[STEPS.indexOf(step) + 1])}>Retry</Button>}</div>}
        </div>
      </main>
    </div>
  );
}
