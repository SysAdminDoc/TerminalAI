/**
 * Decode the two timestamp shapes currently seen at the renderer boundary.
 *
 * Serde's SystemTime is an object, while a future compact wire format may use
 * either epoch seconds or epoch milliseconds. Magnitude keeps both numeric
 * forms unambiguous without making each renderer maintain its own guess.
 */
export function optionalSystemTimeMs(value) {
  if (typeof value === "number") {
    if (!Number.isFinite(value)) return null;
    return Math.abs(value) < 1e11 ? value * 1000 : value;
  }
  if (value && typeof value.secs_since_epoch === "number" && Number.isFinite(value.secs_since_epoch)) {
    return value.secs_since_epoch * 1000 + Math.floor((value.nanos_since_epoch ?? 0) / 1e6);
  }
  return null;
}

export function systemTimeMs(value) {
  return optionalSystemTimeMs(value) ?? Date.now();
}
