import { X } from "lucide-react"

import type { KeysetEntry, TicketStatus } from "@/lib/api"
import { isKeysetExpired } from "@/lib/keysets"
import { Badge } from "@/components/ui/badge"

/** Grayscale ticket status: solid = paid, outline = in progress, X = failed. */
export function StatusBadge({ status }: { status: TicketStatus }) {
  if (status === "paid") return <Badge variant="solid">Paid</Badge>
  if (status === "failed")
    return (
      <Badge variant="outline">
        <X />
        Failed
      </Badge>
    )
  const label = status === "waiting" ? "Awaiting wallet" : "Open"
  return <Badge variant="outline">{label}</Badge>
}

/**
 * Keyset state: solid = issuing new ecash, muted = redeem-only (inactive but
 * its ecash still valid), outline + X = expired.
 */
export function KeysetBadge({ keyset, now }: { keyset: KeysetEntry; now: number }) {
  if (isKeysetExpired(keyset, now))
    return (
      <Badge variant="outline">
        <X />
        Expired
      </Badge>
    )
  if (keyset.active) return <Badge variant="solid">Active</Badge>
  return <Badge variant="muted">Redeem-only</Badge>
}
