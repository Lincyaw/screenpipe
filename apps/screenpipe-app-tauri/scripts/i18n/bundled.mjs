// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

import core from "./locales/zh-CN-core.mjs";
import memory from "./locales/zh-CN-memory.mjs";
import native from "./locales/zh-CN-native.mjs";

// Reviewed source copy is versioned with the app. No provider, credentials or
// runtime text collection is needed to build or use the bundled language.
export const bundledCatalogs = { "zh-CN": { frontend: { ...core, ...memory }, native } };
const displayAttributes = new Set(["pl", "ti", "alt", "arl"]);
const neutralNames = new Set(["screenpipe", "Screenpipe", "macOS", "Windows", "Linux", "Ollama", "Codex", "ChatGPT"]);

/** Map only static extracted UI text, preserving GT elements and variables. */
export function bundledTranslations(source, locale, surface = "frontend") {
  const dictionary = bundledCatalogs[locale]?.[surface];
  if (!dictionary) return {};
  const translated = {};
  for (const [id, message] of Object.entries(source)) {
    let complete = true;
    let changed = false;
    function text(value) {
      const trimmed = value.trim();
      const key = Object.hasOwn(dictionary, value) ? value : trimmed;
      if (Object.hasOwn(dictionary, key)) {
        const replacement = dictionary[key];
        changed ||= replacement !== key;
        return value === key ? replacement : value.slice(0, value.indexOf(trimmed)) + replacement + value.slice(value.indexOf(trimmed) + trimmed.length);
      }
      const prose = trimmed.replace(/\{[^{}]*\}/g, "");
      if (/[a-zA-Z]/.test(prose) && !neutralNames.has(trimmed) && !/^[A-Z\d\s+⌘⌃⌥⇧./_-]{1,12}$/.test(trimmed)) complete = false;
      return value;
    }
    function visit(value) {
      if (typeof value === "string") return text(value);
      if (Array.isArray(value)) return value.map(visit);
      if (!value || typeof value !== "object") return value;
      const result = { ...value };
      if (Object.hasOwn(value, "c")) result.c = visit(value.c);
      if (value.d && typeof value.d === "object") {
        result.d = { ...value.d };
        for (const [key, item] of Object.entries(value.d)) {
          if (displayAttributes.has(key) && typeof item === "string") result.d[key] = text(item);
          else if (key === "b" && item && typeof item === "object") result.d.b = Object.fromEntries(Object.entries(item).map(([branch, content]) => [branch, visit(content)]));
        }
      }
      return result;
    }
    const candidate = visit(message);
    // Do not count an incomplete rich message as translated or replace a full
    // provider translation with a mixture of English and Chinese fragments.
    if (complete && (changed || typeof message === "string" && Object.hasOwn(dictionary, message))) translated[id] = candidate;
  }
  return translated;
}
