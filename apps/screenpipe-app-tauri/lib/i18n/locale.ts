// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

export function resolveLocale(configured: string, system: readonly string[], available: readonly string[], defaultLocale: string): string {
  const supported = new Map(available.map((locale) => [locale.toLowerCase(), locale]));
  const candidates = configured === "system" ? system : [configured];
  for (const candidate of candidates) {
    const normalized = candidate.replaceAll("_", "-").toLowerCase();
    const exact = supported.get(normalized);
    if (exact) return exact;
    if ((normalized === "zh" || normalized === "zh-sg" || /^zh-hans(?:-|$)/.test(normalized)) && supported.has("zh-cn")) return supported.get("zh-cn")!;
    const base = supported.get(normalized.split("-")[0]);
    if (base) return base;
  }
  return defaultLocale;
}
