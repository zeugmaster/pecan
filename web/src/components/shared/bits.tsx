import type { ReactNode } from "react"
import { Check, ChevronLeft, ChevronRight, Copy } from "lucide-react"
import { useState } from "react"

import { formatAge, formatDateTime } from "@/lib/format"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"

export function DetailRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[minmax(7rem,0.34fr)_minmax(0,1fr)] items-baseline gap-3 max-sm:grid-cols-1 max-sm:gap-1">
      <span className="text-sm font-medium text-muted-foreground">{label}</span>
      <div className="min-w-0 text-sm font-medium">{children}</div>
    </div>
  )
}

export function DetailBlock({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid min-w-0 gap-1">
      <span className="text-xs font-medium text-muted-foreground">{label}</span>
      <div className="min-w-0 text-sm">{children}</div>
    </div>
  )
}

export function Field({
  label,
  help,
  htmlFor,
  children,
}: {
  label: string
  help?: string
  htmlFor?: string
  children: ReactNode
}) {
  return (
    <div className="grid gap-2">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {help && <p className="m-0 text-xs leading-5 text-muted-foreground">{help}</p>}
    </div>
  )
}

export function MonoChip({ className, children }: { className?: string; children: ReactNode }) {
  return (
    <span
      className={cn(
        "inline-block max-w-full break-all rounded-sm border bg-muted px-2 py-0.5 font-mono text-xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  )
}

export function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false)
  async function copy() {
    await navigator.clipboard?.writeText(value)
    setCopied(true)
    window.setTimeout(() => setCopied(false), 1600)
  }
  return (
    <Button type="button" variant="outline" size="icon" aria-label={label} onClick={() => void copy()}>
      {copied ? <Check /> : <Copy />}
    </Button>
  )
}

export function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div className="grid justify-items-center gap-1 px-5 py-9 text-center">
      <div className="text-sm font-medium text-foreground">{title}</div>
      <div className="max-w-[58ch] text-sm text-muted-foreground">{body}</div>
    </div>
  )
}

/**
 * A compact single-choice filter in the tab rail's look: muted track, the
 * selected option lifted to the background.
 */
export function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
  label,
  className,
}: {
  value: T
  onChange: (value: T) => void
  options: ReadonlyArray<{ value: T; label: string }>
  /** Accessible name of the group. */
  label: string
  className?: string
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cn(
        "inline-flex w-fit shrink-0 items-stretch rounded-lg bg-muted p-[3px] text-muted-foreground",
        className,
      )}
    >
      {options.map((option) => {
        const selected = option.value === value
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(option.value)}
            className={cn(
              "inline-flex h-[26px] cursor-pointer items-center justify-center whitespace-nowrap rounded-md border border-transparent px-2.5 text-xs font-medium outline-none transition-[color,box-shadow] focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
              selected
                ? "bg-background text-foreground shadow-sm dark:border-input dark:bg-input/30"
                : "hover:text-foreground",
            )}
          >
            {option.label}
          </button>
        )
      })}
    </div>
  )
}

/** Newest-first paging: "26–50 of 1,240" with Newer / Older. */
export function Pager({
  offset,
  limit,
  total,
  onChange,
}: {
  offset: number
  limit: number
  total: number
  onChange: (offset: number) => void
}) {
  const number = new Intl.NumberFormat()
  const last = Math.min(offset + limit, total)
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 px-6 text-sm text-muted-foreground">
      <span>
        {total === 0
          ? "No rows"
          : `${number.format(offset + 1)}–${number.format(last)} of ${number.format(total)}`}
      </span>
      {total > limit && (
        <div className="flex gap-1.5">
          <Button
            variant="outline"
            size="sm"
            disabled={offset === 0}
            onClick={() => onChange(Math.max(0, offset - limit))}
          >
            <ChevronLeft />
            Newer
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={last >= total}
            onClick={() => onChange(offset + limit)}
          >
            Older
            <ChevronRight />
          </Button>
        </div>
      )}
    </div>
  )
}

/** Relative time ("3h ago") with the exact date and time on hover. */
export function Timestamp({ ts, now }: { ts: number; now: number }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <time dateTime={new Date(ts * 1000).toISOString()}>{formatAge(ts, now)}</time>
      </TooltipTrigger>
      <TooltipContent>{formatDateTime(ts)}</TooltipContent>
    </Tooltip>
  )
}

/**
 * Secondary detail behind a chevron row (native <details>, so keyboard and
 * find-in-page work without script). `bleed` rows span a card edge to edge.
 */
export function Disclosure({
  summary,
  meta,
  defaultOpen = false,
  bleed = false,
  children,
}: {
  summary: ReactNode
  /** Short right-aligned hint on the closed row. */
  meta?: ReactNode
  defaultOpen?: boolean
  bleed?: boolean
  children: ReactNode
}) {
  const [open, setOpen] = useState(defaultOpen)
  return (
    <details
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      className="group"
    >
      <summary
        className={cn(
          "flex cursor-pointer list-none items-center gap-2 py-3 text-sm font-medium outline-none transition-colors hover:bg-muted/60 focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:ring-inset [&::-webkit-details-marker]:hidden",
          bleed ? "px-6" : "-mx-2 rounded-md px-2",
        )}
      >
        <ChevronRight className="size-4 shrink-0 text-muted-foreground transition-transform duration-200 group-open:rotate-90" />
        <span className="min-w-0">{summary}</span>
        {meta && (
          <span className="ml-auto min-w-0 truncate pl-3 text-xs font-normal text-muted-foreground max-sm:hidden">
            {meta}
          </span>
        )}
      </summary>
      <div className={cn("pb-5 pt-1", bleed && "px-6")}>{children}</div>
    </details>
  )
}
