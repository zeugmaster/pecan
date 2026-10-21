import { ArrowRight } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"

import type { KeysetEntry } from "@/lib/api"
import { formatDate, formatRelative } from "@/lib/format"
import { keysetState, type KeysetState } from "@/lib/keysets"
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { KeysetBadge } from "@/components/shared/badges"
import { MonoChip } from "@/components/shared/bits"

const DAY = 86_400
const MONTH = 30.44 * DAY
/** Expiries further out than this run off the right edge instead of squashing the rest. */
const MAX_HORIZON = 5 * 365 * DAY

type SegmentKind = "unknown" | "issuing" | "redeem" | "expired" | "planned"

/**
 * Grayscale encodings. Solid = issuing new ecash; hatched = redeem-only;
 * muted = expired; dashed = not observed (start unknown) or not yet real.
 */
const SEGMENT_CLASS: Record<SegmentKind, string> = {
  unknown: "border border-dashed border-muted-foreground/50",
  issuing: "bg-primary",
  redeem:
    "border border-chart-3 bg-card bg-[image:repeating-linear-gradient(135deg,var(--chart-3)_0_1.5px,transparent_1.5px_6px)]",
  expired: "border border-muted-foreground/40 bg-muted",
  planned: "border border-dashed border-foreground bg-card",
}

interface Segment {
  kind: SegmentKind
  from: number
  to: number
}

interface Lane {
  key: string
  keyset?: KeysetEntry
  segments: Segment[]
  /** No final expiry (or beyond the horizon): the bar runs off the right edge. */
  openEnded: boolean
}

/** One keyset's bar: a dashed lead-in while its start is unknown, then issuing → redeem-only → expiry. */
function laneFor(keyset: KeysetEntry, state: KeysetState, lo: number, hi: number): Lane {
  const end = Math.min(keyset.final_expiry ?? hi, hi)
  const begin = Math.min(keyset.first_seen_at ?? lo, end)
  const segments: Segment[] = []
  if (!keyset.start_observed) segments.push({ kind: "unknown", from: lo, to: begin })
  if (state === "expired") {
    segments.push({ kind: "expired", from: begin, to: end })
  } else if (state === "active") {
    segments.push({ kind: "issuing", from: begin, to: end })
  } else {
    const stop = keyset.deactivated_at
    if (stop != null && stop > begin) {
      segments.push({ kind: "issuing", from: begin, to: stop })
      segments.push({ kind: "redeem", from: stop, to: end })
    } else {
      segments.push({ kind: "redeem", from: begin, to: end })
    }
  }
  return {
    key: keyset.id,
    keyset,
    segments: segments.filter((segment) => segment.to > segment.from),
    openEnded: keyset.final_expiry == null || keyset.final_expiry > hi,
  }
}

function domainFor(keysets: KeysetEntry[], now: number, planned: number | null) {
  const starts = keysets.map((keyset) => keyset.first_seen_at ?? now)
  const ends = keysets
    .map((keyset) => keyset.final_expiry)
    .filter((expiry): expiry is number => expiry != null)
    .map((expiry) => Math.min(expiry, now + MAX_HORIZON))
  if (planned != null) ends.push(Math.min(planned, now + MAX_HORIZON))
  const lo = Math.min(now - 30 * DAY, ...starts)
  const hi = Math.max(now + 180 * DAY, lo + 365 * DAY, ...ends)
  const span = hi - lo
  // Room on the left for the "start unknown" lead-in, a little on the right.
  return { lo: lo - span * 0.05, hi: hi + span * 0.03 }
}

/** Month-aligned ticks, at most ~7 across the span. */
function ticksFor(lo: number, hi: number) {
  const step = [1, 2, 3, 6, 12, 24, 60].find((months) => (hi - lo) / MONTH / months <= 7) ?? 120
  const first = new Date(lo * 1000)
  let year = first.getFullYear()
  let month = first.getMonth() + 1
  while ((year * 12 + month) % step !== 0) month += 1
  const ticks: Array<{ ts: number; label: string }> = []
  for (;;) {
    const date = new Date(year, month, 1)
    const ts = date.getTime() / 1000
    if (ts > hi) break
    ticks.push({
      ts,
      label:
        step >= 12
          ? String(date.getFullYear())
          : date.toLocaleDateString(undefined, { month: "short", year: "2-digit" }),
    })
    month += step
  }
  return ticks
}

/** Shared column template: id · track · state. Collapses to a stacked row on phones. */
const ROW = "grid gap-1.5 sm:grid-cols-[8.5rem_minmax(0,1fr)_14rem] sm:items-center sm:gap-4"

export function KeyTimeline({
  keysets,
  now,
  plannedExpiry,
}: {
  /** Already filtered and ordered by the caller. */
  keysets: KeysetEntry[]
  now: number
  /** Preview of a keyset created now with this final expiry. */
  plannedExpiry: number | null
}) {
  const { lo, hi } = domainFor(keysets, now, plannedExpiry)
  const x = (ts: number) => ((Math.min(Math.max(ts, lo), hi) - lo) / (hi - lo)) * 100
  const ticks = ticksFor(lo, hi).filter((tick) => {
    const position = x(tick.ts)
    return position > 3 && position < 97 && Math.abs(position - x(now)) > 7
  })

  const lanes = keysets.map((keyset) => laneFor(keyset, keysetState(keyset, now), lo, hi))
  const planned: Lane | null =
    plannedExpiry != null
      ? {
          key: "planned",
          segments: [{ kind: "planned", from: now, to: Math.min(plannedExpiry, hi) }],
          openEnded: plannedExpiry > hi,
        }
      : null

  return (
    <div className="grid gap-3">
      <div className={ROW} aria-hidden>
        <div className="hidden sm:block" />
        <div className="relative h-4 text-[11px] text-muted-foreground">
          {ticks.map((tick) => (
            <span
              key={tick.ts}
              className="absolute -translate-x-1/2 whitespace-nowrap"
              style={{ left: `${x(tick.ts)}%` }}
            >
              {tick.label}
            </span>
          ))}
          <span
            className="absolute -translate-x-1/2 whitespace-nowrap font-semibold text-foreground"
            style={{ left: `${x(now)}%` }}
          >
            Today
          </span>
        </div>
        <div className="hidden sm:block" />
      </div>

      <div role="list" className="grid gap-3">
        {planned && (
          <LaneRow
            lane={planned}
            x={x}
            ticks={ticks}
            now={now}
            label={<span className="font-mono text-xs text-muted-foreground">new keyset</span>}
            state={
              <span className="flex items-center gap-2 text-sm text-muted-foreground">
                <Badge variant="outline">Preview</Badge>
                <span className="truncate">until {formatDate(plannedExpiry!)}</span>
              </span>
            }
            description={`Preview: a keyset created today, expiring ${formatDate(plannedExpiry!)}`}
          />
        )}
        {lanes.map((lane) => (
          <KeysetLane key={lane.key} lane={lane} x={x} ticks={ticks} now={now} />
        ))}
      </div>
    </div>
  )
}

function KeysetLane({
  lane,
  x,
  ticks,
  now,
}: {
  lane: Lane
  x: (ts: number) => number
  ticks: Array<{ ts: number }>
  now: number
}) {
  const keyset = lane.keyset!
  const state = keysetState(keyset, now)
  const expiry = keyset.final_expiry
  const until =
    expiry == null
      ? "no expiry"
      : state === "expired"
        ? `expired ${formatDate(expiry)}`
        : `until ${formatDate(expiry)}`
  const started = keyset.first_seen_at
    ? keyset.start_observed
      ? formatDate(keyset.first_seen_at)
      : `before ${formatDate(keyset.first_seen_at)}`
    : "unknown"
  const stopped =
    state === "active"
      ? "still issuing"
      : keyset.deactivated_at
        ? formatDate(keyset.deactivated_at)
        : keyset.first_seen_at
          ? `before ${formatDate(keyset.first_seen_at)}`
          : "unknown"

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <LaneRow
          lane={lane}
          x={x}
          ticks={ticks}
          now={now}
          tabIndex={0}
          label={<MonoChip className="truncate">{keyset.id.slice(0, 12)}…</MonoChip>}
          state={
            <span className="flex items-center gap-2 text-sm text-muted-foreground">
              <KeysetBadge keyset={keyset} now={now} />
              <span className="truncate">{until}</span>
            </span>
          }
          description={`Keyset ${keyset.id}: started ${started}, ${
            state === "active" ? "issuing new ecash" : `stopped issuing ${stopped}`
          }, ${expiry == null ? "no expiry" : `ecash redeemable until ${formatDate(expiry)}`}.`}
        />
      </TooltipTrigger>
      <TooltipContent side="bottom" className="grid gap-0.5 text-left">
        <span className="font-mono">{keyset.id}</span>
        <span>Started: {started}</span>
        <span>Stopped issuing: {stopped}</span>
        <span>
          Redeemable until:{" "}
          {expiry == null ? "no expiry" : `${formatDate(expiry)} (${formatRelative(expiry, now)})`}
        </span>
        <span>Input fee: {keyset.input_fee_ppk} ppk</span>
      </TooltipContent>
    </Tooltip>
  )
}

/** One row of the list; forwards DOM props so it can be a tooltip trigger itself. */
function LaneRow({
  lane,
  x,
  ticks,
  now,
  label,
  state,
  description,
  className,
  ...props
}: {
  lane: Lane
  x: (ts: number) => number
  ticks: Array<{ ts: number }>
  now: number
  label: ReactNode
  state: ReactNode
  description: string
} & ComponentProps<"div">) {
  const last = lane.segments.length - 1
  return (
    <div
      role="listitem"
      aria-label={description}
      className={cn(
        ROW,
        "rounded-md outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
        className,
      )}
      {...props}
    >
      <div className="flex min-w-0 items-center justify-between gap-2 sm:contents">
        <div className="min-w-0 sm:order-1">{label}</div>
        <div className="min-w-0 sm:order-3">{state}</div>
      </div>
      <div className="relative h-5 sm:order-2" aria-hidden>
        {ticks.map((tick) => (
          <span
            key={tick.ts}
            className="absolute inset-y-0 w-px bg-border"
            style={{ left: `${x(tick.ts)}%` }}
          />
        ))}
        {lane.segments.map((segment, index) => (
          <span
            key={`${segment.kind}-${index}`}
            className={cn(
              "absolute inset-y-0",
              SEGMENT_CLASS[segment.kind],
              index === 0 && "rounded-l-sm",
              index === last && !lane.openEnded && "rounded-r-sm",
            )}
            style={{
              left: `${x(segment.from)}%`,
              width: `${x(segment.to) - x(segment.from)}%`,
            }}
          />
        ))}
        {lane.openEnded && lane.segments.length > 0 && (
          <span
            className={cn(
              "absolute inset-y-0 right-0.5 flex items-center",
              lane.segments[last].kind === "issuing" ? "text-primary-foreground" : "text-foreground",
            )}
          >
            <ArrowRight className="size-3" />
          </span>
        )}
        <span className="absolute -inset-y-1 w-px bg-foreground" style={{ left: `${x(now)}%` }} />
      </div>
    </div>
  )
}

/** Swatches for the encodings above, in the same order operators read a bar. */
export function KeyTimelineLegend({ showPlanned }: { showPlanned: boolean }) {
  const items: Array<{ kind: SegmentKind; label: string }> = [
    { kind: "issuing", label: "Issuing new ecash" },
    { kind: "redeem", label: "Redeem-only" },
    { kind: "expired", label: "Expired" },
    { kind: "unknown", label: "Before this console saw it" },
    ...(showPlanned ? [{ kind: "planned" as const, label: "If rotated today" }] : []),
  ]
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted-foreground">
      {items.map((item) => (
        <span key={item.kind} className="inline-flex items-center gap-1.5">
          <span className={cn("h-2.5 w-4 rounded-[3px]", SEGMENT_CLASS[item.kind])} />
          {item.label}
        </span>
      ))}
      <span className="inline-flex items-center gap-1.5">
        <span className="h-3 w-px bg-foreground" />
        Today
      </span>
      <span className="inline-flex items-center gap-1.5">
        <ArrowRight className="size-3 text-foreground" />
        No expiry
      </span>
    </div>
  )
}
