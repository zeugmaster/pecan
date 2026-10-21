export function formatAmount(amount: number, unit: string) {
  return `${new Intl.NumberFormat().format(amount)} ${unit.toUpperCase()}`
}

export function formatSignedAmount(amount: number, unit: string) {
  if (amount < 0) return `-${formatAmount(Math.abs(amount), unit)}`
  return formatAmount(amount, unit)
}

export function compactNumber(amount: number) {
  return new Intl.NumberFormat(undefined, { notation: "compact" }).format(amount)
}

export function formatAge(then: number, now: number) {
  if (then >= now) return "just now"
  const seconds = now - then
  if (seconds < 60) return `${seconds}s ago`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86_400)}d ago`
}

export function formatExpiry(expiry: number | null | undefined, now: number) {
  if (!expiry) return "-"
  if (expiry <= now) return `expired ${formatAge(expiry, now)}`
  const delta = expiry - now
  if (delta < 3600) return `in ${Math.floor(delta / 60)}m`
  if (delta < 86_400) return `in ${Math.floor(delta / 3600)}h`
  return `in ${Math.floor(delta / 86_400)}d`
}

export function formatCountdown(until: number, now: number) {
  const delta = Math.max(until - now, 0)
  if (delta >= 3600) return `${Math.floor(delta / 3600)}h ${Math.floor((delta % 3600) / 60)}m`
  if (delta >= 60) return `${Math.floor(delta / 60)}m ${delta % 60}s`
  return `${delta}s`
}

function isThisYear(ts: number) {
  return new Date(ts * 1000).getFullYear() === new Date().getFullYear()
}

/** Date and time; the year appears once it is not the current one. */
export function formatDateTime(ts: number) {
  return new Date(ts * 1000).toLocaleString(undefined, {
    year: isThisYear(ts) ? undefined : "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  })
}

/** "Oct 20, 2027" — calendar dates (expiries, starts) always carry the year. */
export function formatDate(ts: number) {
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })
}

/** Coarse length for long horizons: "45 min", "5 h", "12 days", "11 months", "2 years". */
export function formatSpan(seconds: number) {
  const secs = Math.abs(seconds)
  if (secs < 3600) return `${Math.max(1, Math.round(secs / 60))} min`
  if (secs < 2 * 86_400) return `${Math.round(secs / 3600)} h`
  const days = Math.round(secs / 86_400)
  if (days < 60) return `${days} days`
  const months = Math.round(days / 30.44)
  if (months < 24) return `${months} months`
  return `${Math.round(days / 365.25)} years`
}

/** "in 11 months" / "3 days ago" */
export function formatRelative(ts: number, now: number) {
  return ts >= now ? `in ${formatSpan(ts - now)}` : `${formatSpan(now - ts)} ago`
}
