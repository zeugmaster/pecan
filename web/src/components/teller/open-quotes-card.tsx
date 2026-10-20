import { useState } from "react"

import type { OpenQuoteSummary, TicketKind } from "@/lib/api"
import { formatAge, formatAmount, formatExpiry } from "@/lib/format"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { StatusBadge } from "@/components/shared/badges"
import { EmptyState, MonoChip, SegmentedControl } from "@/components/shared/bits"

/** Rows shown before "Show all": the list stays scannable on a busy day. */
const COLLAPSED_ROWS = 10

const KINDS = [
  { value: "all", label: "All" },
  { value: "incoming", label: "Deposits" },
  { value: "outgoing", label: "Payouts" },
] as const

/**
 * Read-only situational awareness: which wallet-created quotes are open right
 * now. Ids arrive truncated from the server (leading characters only) — the
 * rows are deliberately not actionable, settling always goes through the
 * match input with a code from the customer's wallet.
 */
export function OpenQuotesCard({
  quotes,
  now,
}: {
  quotes: OpenQuoteSummary[]
  now: number
}) {
  const [kind, setKind] = useState<TicketKind | "all">("all")
  const [expanded, setExpanded] = useState(false)
  const filtered = kind === "all" ? quotes : quotes.filter((quote) => quote.kind === kind)
  const visible = expanded ? filtered : filtered.slice(0, COLLAPSED_ROWS)

  return (
    <Card>
      <CardHeader>
        <CardTitle>Open quotes</CardTitle>
        <CardDescription>
          {quotes.length === 0
            ? "None right now"
            : `${quotes.length} open · newest first`}
        </CardDescription>
        {quotes.length > 0 && (
          <CardAction>
            <SegmentedControl label="Show" value={kind} onChange={setKind} options={KINDS} />
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="px-0">
        {quotes.length === 0 ? (
          <EmptyState
            title="No open quotes"
            body="Customers create quotes in their wallet; they appear here."
          />
        ) : filtered.length === 0 ? (
          <EmptyState
            title={kind === "incoming" ? "No open deposits" : "No open payouts"}
            body="Switch to All to see every open quote."
          />
        ) : (
          <Table className="min-w-[560px]">
            <TableHeader>
              <TableRow>
                <TableHead className="pl-6">Quote</TableHead>
                <TableHead>Kind</TableHead>
                <TableHead>Amount</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Created</TableHead>
                <TableHead className="pr-6">Expires</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {visible.map((quote, index) => (
                <TableRow key={`${quote.prefix}-${index}`}>
                  <TableCell className="pl-6">
                    <MonoChip>{quote.prefix}…</MonoChip>
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {quote.kind === "incoming" ? "Deposit" : "Payout"}
                  </TableCell>
                  <TableCell className="font-medium">
                    {formatAmount(quote.amount, quote.unit)}
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={quote.status} />
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {formatAge(quote.created_at, now)}
                  </TableCell>
                  <TableCell className="pr-6 text-muted-foreground">
                    {formatExpiry(quote.expires_at, now)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
        {filtered.length > COLLAPSED_ROWS && (
          <div className="px-6 pt-2">
            <Button variant="ghost" size="sm" className="w-full" onClick={() => setExpanded(!expanded)}>
              {expanded ? "Show fewer" : `Show all ${filtered.length}`}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
