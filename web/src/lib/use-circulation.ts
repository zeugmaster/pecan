import { useEffect, useState } from "react"

import { fetchCirculation, type AppSnapshot, type CirculationDay } from "@/lib/api"

/** Changes whenever a settlement lands or the day rolls over, so totals refetch. */
export function settledKey(snapshot: AppSnapshot) {
  const day = new Date(snapshot.now * 1000).toDateString()
  return `${snapshot.summary.mint_count}:${snapshot.summary.melt_count}:${day}`
}

/** Daily settled totals for the trailing `days` (undefined = all time). */
export function useCirculation(days: number | undefined, refreshKey: string) {
  const [data, setData] = useState<CirculationDay[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    fetchCirculation(days)
      .then((result) => {
        if (cancelled) return
        setData(result)
        setError(null)
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : "Could not load totals.")
      })
    return () => {
      cancelled = true
    }
  }, [days, refreshKey])

  return { data, error }
}
