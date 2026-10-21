import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"

import type { CirculationDay } from "@/lib/api"
import { compactNumber, formatAmount, formatSignedAmount } from "@/lib/format"
import { useCirculation } from "@/lib/use-circulation"
import { Skeleton } from "@/components/ui/skeleton"
import { EmptyState } from "@/components/shared/bits"

export const CHART_RANGES = [
  { value: "7", label: "7D" },
  { value: "30", label: "30D" },
  { value: "90", label: "90D" },
  { value: "365", label: "1Y" },
  { value: "all", label: "All" },
] as const

export type ChartRange = (typeof CHART_RANGES)[number]["value"]

/** Net settled per day (deposits minus payouts, running), one point per day. */
export function CirculationChart({
  unit,
  range,
  refreshKey,
}: {
  unit: string
  range: ChartRange
  refreshKey: string
}) {
  const { data, error } = useCirculation(range === "all" ? undefined : Number(range), refreshKey)

  if (!data) {
    return error ? (
      <p className="m-0 text-sm text-muted-foreground">Could not load the settled history: {error}</p>
    ) : (
      <Skeleton className="h-[300px] w-full" />
    )
  }
  if (data.length === 0) {
    return (
      <EmptyState
        title="No circulation history yet"
        body="Settled deposits and payouts will build this graph over time."
      />
    )
  }

  const crossesYears =
    new Date(data[0].start * 1000).getFullYear() !==
    new Date(data[data.length - 1].start * 1000).getFullYear()
  const chartData = data.map((day) => ({
    ...day,
    label: new Date(day.start * 1000).toLocaleDateString(undefined, {
      year: crossesYears ? "2-digit" : undefined,
      month: "short",
      day: "numeric",
    }),
  }))

  return (
    <div className="h-[300px] w-full">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={chartData} margin={{ left: 2, right: 12, top: 14, bottom: 0 }}>
          <defs>
            <linearGradient id="circulationFill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="var(--chart-3)" stopOpacity={0.5} />
              <stop offset="95%" stopColor="var(--chart-3)" stopOpacity={0.05} />
            </linearGradient>
          </defs>
          <CartesianGrid stroke="var(--border)" strokeDasharray="3 3" vertical={false} />
          <XAxis
            dataKey="label"
            tickLine={false}
            axisLine={false}
            tickMargin={10}
            minTickGap={24}
            tick={{ fill: "var(--muted-foreground)", fontSize: 12 }}
          />
          <YAxis
            width={64}
            tickLine={false}
            axisLine={false}
            tick={{ fill: "var(--muted-foreground)", fontSize: 12 }}
            tickFormatter={(value) => compactNumber(Number(value))}
          />
          <Tooltip content={<ChartTooltip unit={unit} />} />
          <Area
            type="monotone"
            dataKey="circulation"
            stroke="var(--chart-1)"
            strokeWidth={2}
            fill="url(#circulationFill)"
            dot={false}
            activeDot={{ r: 4, fill: "var(--chart-1)", stroke: "var(--background)" }}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  )
}

function ChartTooltip({
  active,
  payload,
  unit,
}: {
  active?: boolean
  payload?: Array<{ payload: CirculationDay }>
  unit: string
}) {
  if (!active || !payload?.length) return null
  const day = payload[0].payload
  return (
    <div className="rounded-md border bg-popover px-3 py-2 text-popover-foreground shadow-md">
      <div className="text-xs font-medium text-muted-foreground">
        {new Date(day.start * 1000).toLocaleDateString(undefined, {
          weekday: "short",
          year: "numeric",
          month: "short",
          day: "numeric",
        })}
      </div>
      <div className="mt-1 text-sm font-semibold">{formatSignedAmount(day.circulation, unit)}</div>
      <div className="text-xs text-muted-foreground">
        In {formatAmount(day.deposits, unit)} ({day.deposit_count}) · Out{" "}
        {formatAmount(day.payouts, unit)} ({day.payout_count})
      </div>
    </div>
  )
}
