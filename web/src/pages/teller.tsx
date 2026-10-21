import { useEffect, useRef, useState } from "react"
import { toast } from "sonner"

import { ApiRequestError, matchQuote, type CirculationDay, type Ticket } from "@/lib/api"
import { formatAmount } from "@/lib/format"
import { navigate } from "@/lib/router"
import { useSnapshot } from "@/lib/snapshot"
import { settledKey, useCirculation } from "@/lib/use-circulation"
import { useNow } from "@/lib/use-now"
import { buttonVariants } from "@/components/ui/button"
import { ActivityTable } from "@/components/shared/activity-table"
import { MatchCard } from "@/components/teller/match-card"
import { MatchedQuoteCard } from "@/components/teller/matched-quote-card"
import { OpenQuotesCard } from "@/components/teller/open-quotes-card"

/**
 * The till, match-first: the operator resolves the customer's quote by its id
 * (typed tail or scanned), works the matched card, and never settles anything
 * straight from the open-quote list — its ids are truncated server-side.
 */
export function TellerPage() {
  const { snapshot, refresh } = useSnapshot()
  const now = useNow(snapshot.now)
  const [matched, setMatched] = useState<Ticket | null>(null)
  const matchedCode = useRef<string | null>(null)
  const today = useCirculation(1, settledKey(snapshot)).data?.[0]
  const unit = snapshot.setup.unit

  function showMatch(ticket: Ticket, code: string) {
    matchedCode.current = code
    setMatched(ticket)
  }

  function clearMatch() {
    matchedCode.current = null
    setMatched(null)
  }

  // Keep the matched card current as the wallet acts (SSE refreshes the
  // snapshot; re-resolve quietly so e.g. a withdrawal advances from
  // "awaiting wallet" to "ready to pay out" while the card is open).
  useEffect(() => {
    const code = matchedCode.current
    if (!code) return
    let cancelled = false
    matchQuote(code)
      .then((ticket) => {
        if (!cancelled) setMatched(ticket)
      })
      .catch((err) => {
        if (cancelled) return
        if (err instanceof ApiRequestError && err.status !== 502) {
          clearMatch()
          toast(err.message)
        }
        // 502 (mint briefly unreachable) or network hiccup: keep the card,
        // the next snapshot tick retries.
      })
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot])

  return (
    <div className="mx-auto grid w-full max-w-2xl gap-6">
      {matched ? (
        <MatchedQuoteCard
          ticket={matched}
          now={now}
          onDismiss={clearMatch}
          onSettled={async () => {
            clearMatch()
            await refresh()
          }}
        />
      ) : (
        <MatchCard onMatched={showMatch} />
      )}

      <OpenQuotesCard quotes={snapshot.open_quotes} now={now} />

      <ActivityTable
        title="Recent activity"
        description={today && unit ? todaySummary(today, unit) : undefined}
        tickets={snapshot.recent_done.slice(0, 10)}
        now={now}
        showNotes
        action={
          <a
            href="/#activity"
            className={buttonVariants({ variant: "outline", size: "sm" })}
            onClick={(event) => {
              event.preventDefault()
              navigate("/#activity")
            }}
          >
            Full history
          </a>
        }
      />
    </div>
  )
}

/** "Today: 3 deposits (1,200 ORA in) · 1 payout (200 ORA out)" — for squaring the drawer. */
function todaySummary(today: CirculationDay, unit: string) {
  const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`
  return `Today: ${plural(today.deposit_count, "deposit")} (${formatAmount(today.deposits, unit)} in) · ${plural(
    today.payout_count,
    "payout",
  )} (${formatAmount(today.payouts, unit)} out)`
}
