import { useEffect, useMemo, useState } from "react"
import { Download, Search } from "lucide-react"

import {
  fetchHistory,
  historyCsvUrl,
  type AppSnapshot,
  type HistoryFilters,
  type HistoryPage,
  type TicketKind,
} from "@/lib/api"
import { formatAmount, formatSignedAmount } from "@/lib/format"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Skeleton } from "@/components/ui/skeleton"
import { TicketTable } from "@/components/shared/activity-table"
import { EmptyState, Pager, SegmentedControl } from "@/components/shared/bits"

const PAGE_SIZE = 25

const KINDS = [
  { value: "all", label: "All" },
  { value: "incoming", label: "Deposits" },
  { value: "outgoing", label: "Payouts" },
] as const

const STATUSES = [
  { value: "all", label: "All" },
  { value: "paid", label: "Paid" },
  { value: "failed", label: "Failed" },
] as const

/** `yyyy-mm-dd` from a date input → unix seconds of that local midnight. */
function localMidnight(date: string, plusDays = 0) {
  const [year, month, day] = date.split("-").map(Number)
  return Math.floor(new Date(year, month - 1, day + plusDays).getTime() / 1000)
}

export function ActivityTab({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const unit = snapshot.setup.unit
  const [kind, setKind] = useState<TicketKind | "all">("all")
  const [status, setStatus] = useState<"paid" | "failed" | "all">("all")
  const [search, setSearch] = useState("")
  const [query, setQuery] = useState("")
  const [from, setFrom] = useState("")
  const [to, setTo] = useState("")
  const [offset, setOffset] = useState(0)
  const [page, setPage] = useState<HistoryPage | null>(null)
  const [error, setError] = useState<string | null>(null)

  // Debounce typing; any filter change starts over at the newest page.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setQuery(search.trim())
      setOffset(0)
    }, 250)
    return () => window.clearTimeout(timer)
  }, [search])

  const filters: HistoryFilters = useMemo(
    () => ({
      kind: kind === "all" ? undefined : kind,
      status: status === "all" ? undefined : status,
      q: query || undefined,
      from: from ? localMidnight(from) : undefined,
      // The end date is inclusive: stop at the following midnight.
      to: to ? localMidnight(to, 1) : undefined,
    }),
    [kind, status, query, from, to],
  )

  // Refetch when a ticket closes anywhere (the SSE snapshot carries the newest).
  const latest = snapshot.recent_done[0]
  const refreshKey = latest ? `${latest.id}:${latest.status}` : ""

  useEffect(() => {
    let cancelled = false
    fetchHistory(filters, offset, PAGE_SIZE)
      .then((result) => {
        if (cancelled) return
        setPage(result)
        setError(null)
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : "Could not load the history.")
      })
    return () => {
      cancelled = true
    }
  }, [filters, offset, refreshKey])

  function filterBy<T>(set: (value: T) => void) {
    return (value: T) => {
      set(value)
      setOffset(0)
    }
  }

  const filtered = kind !== "all" || status !== "all" || query !== "" || from !== "" || to !== ""
  const summary = page?.summary

  return (
    <Card>
      <CardHeader>
        <CardTitle>Activity</CardTitle>
        <CardDescription>
          Every settled deposit and payout, and every failed or voided quote, newest first.
        </CardDescription>
        <CardAction>
          <a
            href={historyCsvUrl(filters)}
            download
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            <Download />
            Export CSV
          </a>
        </CardAction>
      </CardHeader>

      <CardContent className="grid gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative min-w-[14rem] flex-1">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              aria-label="Search activity"
              className="pl-8"
              placeholder="Quote id, ticket, note, or user"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </div>
          <SegmentedControl label="Kind" value={kind} onChange={filterBy(setKind)} options={KINDS} />
          <SegmentedControl
            label="Status"
            value={status}
            onChange={filterBy(setStatus)}
            options={STATUSES}
          />
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="activity-from" className="text-xs text-muted-foreground">
              From
            </Label>
            <Input
              id="activity-from"
              type="date"
              className="w-auto"
              value={from}
              max={to || undefined}
              onChange={(event) => filterBy(setFrom)(event.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="activity-to" className="text-xs text-muted-foreground">
              To
            </Label>
            <Input
              id="activity-to"
              type="date"
              className="w-auto"
              value={to}
              min={from || undefined}
              onChange={(event) => filterBy(setTo)(event.target.value)}
            />
          </div>
          {filtered && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setKind("all")
                setStatus("all")
                setSearch("")
                setQuery("")
                setFrom("")
                setTo("")
                setOffset(0)
              }}
            >
              Clear filters
            </Button>
          )}
        </div>
        {summary && unit && (summary.mint_count > 0 || summary.melt_count > 0) && (
          <p className="m-0 text-sm text-muted-foreground">
            Paid {filtered ? "in this selection" : "in total"}: {summary.mint_count} deposit
            {summary.mint_count === 1 ? "" : "s"} ({formatAmount(summary.minted_amount, unit)}) ·{" "}
            {summary.melt_count} payout{summary.melt_count === 1 ? "" : "s"} (
            {formatAmount(summary.melted_amount, unit)}) · net{" "}
            <span className="font-medium text-foreground">
              {formatSignedAmount(summary.net_issued, unit)}
            </span>
          </p>
        )}
      </CardContent>

      <CardContent className="px-0">
        {!page ? (
          error ? (
            <p className="m-0 px-6 text-sm text-muted-foreground">{error}</p>
          ) : (
            <div className="grid gap-2 px-6">
              {Array.from({ length: 5 }, (_, index) => (
                <Skeleton key={index} className="h-9" />
              ))}
            </div>
          )
        ) : page.tickets.length === 0 ? (
          <EmptyState
            title={filtered ? "Nothing matches these filters" : "No settled operations yet"}
            body={
              filtered
                ? "Try a shorter search or a wider date range."
                : "Completed deposits and payouts will appear here."
            }
          />
        ) : (
          <TicketTable tickets={page.tickets} now={now} showOperator showNotes />
        )}
      </CardContent>

      {page && page.total > 0 && (
        <Pager offset={page.offset} limit={page.limit} total={page.total} onChange={setOffset} />
      )}
    </Card>
  )
}
