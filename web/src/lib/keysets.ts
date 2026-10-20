import type { KeysetEntry } from "@/lib/api"

/** cdk treats a keyset as expired strictly after its final expiry. */
export function isKeysetExpired(keyset: KeysetEntry, now: number) {
  return keyset.final_expiry != null && keyset.final_expiry < now
}

export type KeysetState = "active" | "redeem" | "expired"

export function keysetState(keyset: KeysetEntry, now: number): KeysetState {
  if (isKeysetExpired(keyset, now)) return "expired"
  return keyset.active ? "active" : "redeem"
}

const STATE_RANK: Record<KeysetState, number> = { active: 0, redeem: 1, expired: 2 }

/** Issuing keyset first, then redeem-only, then expired; newest first within each. */
export function sortKeysets(keysets: KeysetEntry[], now: number) {
  const recency = (keyset: KeysetEntry) =>
    keyset.deactivated_at ?? keyset.first_seen_at ?? keyset.final_expiry ?? 0
  return [...keysets].sort(
    (a, b) =>
      STATE_RANK[keysetState(a, now)] - STATE_RANK[keysetState(b, now)] ||
      recency(b) - recency(a) ||
      a.id.localeCompare(b.id),
  )
}

/**
 * Final expiry for a keyset created now that should last `months`: rounded up
 * to the next UTC midnight so the timestamp is stable and reads as a date.
 */
export function plannedExpiry(now: number, months: number) {
  const date = new Date(now * 1000)
  date.setUTCMonth(date.getUTCMonth() + months)
  date.setUTCHours(24, 0, 0, 0)
  return Math.floor(date.getTime() / 1000)
}
