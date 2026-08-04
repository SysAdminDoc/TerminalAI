import { FluentBundle, FluentResource } from "@fluent/bundle";
import catalogSource from "./i18n/terminalai.ftl?raw";

export const DEFAULT_LOCALE = "en-US";
export const LOCALE = typeof navigator !== "undefined" && navigator.language
  ? navigator.language
  : DEFAULT_LOCALE;

const bundle = new FluentBundle([LOCALE], { useIsolating: false });
bundle.addResource(new FluentResource(catalogSource));

const pluralRules = new Intl.PluralRules(LOCALE);
const relativeTime = new Intl.RelativeTimeFormat(LOCALE, {
  numeric: "always",
  style: "short",
});

function hasMessage(id) {
  return Boolean(bundle.getMessage(id)?.value);
}

export function t(id, args = {}) {
  const message = bundle.getMessage(id);
  if (!message?.value) return id;
  const errors = [];
  return bundle.formatPattern(message.value, args, errors);
}

export function countMessage(prefix, count, args = {}) {
  const numericCount = Number.isFinite(Number(count)) ? Number(count) : 0;
  const category = pluralRules.select(numericCount);
  const preferred = `${prefix}-${category}`;
  const messageId = hasMessage(preferred) ? preferred : `${prefix}-other`;
  return t(messageId, { ...args, count: numericCount });
}

export function relativeDwell(value, now = Date.now()) {
  const elapsedSeconds = Math.max(0, Math.floor((now - systemTimeMs(value)) / 1000));
  if (elapsedSeconds === 0) return t("relative-now");
  if (elapsedSeconds < 60) return relativeTime.format(-elapsedSeconds, "second");
  const elapsedMinutes = Math.floor(elapsedSeconds / 60);
  if (elapsedMinutes < 60) return relativeTime.format(-elapsedMinutes, "minute");
  const elapsedHours = Math.floor(elapsedMinutes / 60);
  if (elapsedHours < 24) return relativeTime.format(-elapsedHours, "hour");
  const elapsedDays = Math.floor(elapsedHours / 24);
  if (elapsedDays < 7) return relativeTime.format(-elapsedDays, "day");
  return relativeTime.format(-Math.floor(elapsedDays / 7), "week");
}

function systemTimeMs(value) {
  if (typeof value === "number") return value;
  if (value && typeof value.secs_since_epoch === "number") {
    return value.secs_since_epoch * 1000 + Math.floor((value.nanos_since_epoch ?? 0) / 1e6);
  }
  return Date.now();
}

export function localizeDom(root = document) {
  for (const element of root.querySelectorAll("[data-i18n]")) {
    element.textContent = t(element.dataset.i18n);
  }
  for (const element of root.querySelectorAll("[data-i18n-aria-label]")) {
    element.setAttribute("aria-label", t(element.dataset.i18nAriaLabel));
  }
  for (const element of root.querySelectorAll("[data-i18n-title]")) {
    element.title = t(element.dataset.i18nTitle);
  }
  for (const element of root.querySelectorAll("[data-i18n-placeholder]")) {
    element.placeholder = t(element.dataset.i18nPlaceholder);
  }
}
