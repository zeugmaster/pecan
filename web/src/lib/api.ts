export type TicketKind = "incoming" | "outgoing"
export type TicketStatus = "waiting" | "pending" | "paid" | "failed"

export interface KeysetEntry {
  id: string
  unit: string
  active: boolean
  input_fee_ppk: number
  /** Unix seconds after which the mint refuses this keyset's ecash. */
  final_expiry?: number | null
  /** When this processor first saw the keyset in the mint's list. */
  first_seen_at?: number | null
  /** False when the keyset predates the processor's first look at the mint. */
  start_observed: boolean
  /** When the keyset was seen to stop issuing (go inactive). */
  deactivated_at?: number | null
}

export type CheckStatus = "ok" | "warn" | "fail" | "unknown"

/** One row of the mint-attachment checklist, evaluated server-side. */
export interface ChecklistItem {
  id: string
  status: CheckStatus
  title: string
  detail: string
  /** Plain-language fix, present when the check is not ok. */
  remedy?: string | null
}

export interface SelfTestLeg {
  ok: boolean
  detail: string
  remedy?: string | null
}

/** Result of the end-to-end self-test (one deposit + one payout probe). */
export interface SelfTestOutcome {
  ran_at: number
  ok: boolean
  latency_ms?: number | null
  deposit: SelfTestLeg
  payout: SelfTestLeg
  mint_quote_ttl_secs?: number | null
  melt_quote_ttl_secs?: number | null
  warnings: string[]
}

/** Read-only identity of the attached mint, from its /v1/info. */
export interface MintIdentity {
  name?: string | null
  description?: string | null
  icon_url?: string | null
  version?: string | null
}

export interface SetupState {
  /** The single unit this install serves; "" until setup. */
  unit: string
  unit_locked: boolean
  /** Whether the unit field is editable (not locked, no real tickets yet). */
  unit_change_allowed: boolean
  method: string
  mint_url: string
  /** host[:port] the mint uses to reach this processor's gRPC endpoint. */
  advertised_grpc: string
  /** Where this processor actually listens for the mint. */
  grpc_bind: string
  grpc_tls: boolean
  /** Host-published gRPC port, for the attachment prefill. */
  published_grpc_port: number
  attached: boolean
  setup_complete: boolean
}

export interface AttachSignals {
  last_settings_at?: number | null
  stream_attached_at?: number | null
}

export interface Ticket {
  id: string
  short_id: string
  /** The mint's quote id — what the customer's wallet displays. */
  quote_id?: string | null
  kind: TicketKind
  kind_label: string
  amount: number
  unit: string
  status: TicketStatus
  status_label: string
  created_at: number
  paid_at?: number | null
  expires_at?: number | null
  voided_at?: number | null
  /** Settled or voided: when the ticket left the open list. */
  closed_at: number
  /** The console user who settled or voided it. */
  operator?: string | null
  description?: string | null
  notes?: string | null
}

/**
 * Redacted row of the teller's open-quote list. Only the leading characters of
 * the quote id are shipped — matching requires the trailing characters, which
 * must come from the customer's wallet.
 */
export interface OpenQuoteSummary {
  prefix: string
  kind: TicketKind
  kind_label: string
  amount: number
  unit: string
  status: TicketStatus
  status_label: string
  created_at: number
  expires_at?: number | null
}

/** One calendar day (viewer-local) of settled activity for the unit. */
export interface CirculationDay {
  /** Unix seconds of the day's local midnight. */
  start: number
  deposits: number
  deposit_count: number
  payouts: number
  payout_count: number
  /** Net settled since the beginning, at the end of the day. */
  circulation: number
}

export interface SettledSummary {
  mint_count: number
  melt_count: number
  minted_amount: number
  melted_amount: number
  net_issued: number
}

export interface UserEntry {
  username: string
  created_at: number
}

export interface AppSnapshot {
  now: number
  session: { username: string; must_change_password: boolean }
  users: UserEntry[]
  demo_password_active: boolean
  password_min_length: number
  /** Image/build version stamped at build time; "dev" outside CI images. */
  version: string
  setup: SetupState
  attach_signals: AttachSignals
  checklist: ChecklistItem[]
  self_test?: SelfTestOutcome | null
  /** The mint.toml fragment for the operator's cdk-mintd; null until setup. */
  snippet?: string | null
  mint_identity?: MintIdentity | null
  /** The configured unit's keysets at the mint (read-only). */
  keysets: KeysetEntry[]
  keysets_error?: string | null
  summary: SettledSummary
  open_quotes: OpenQuoteSummary[]
  recent_done: Ticket[]
  /** One-time notice: this install previously managed its own mint. */
  migrated_from_managed: boolean
}

export class ApiRequestError extends Error {
  status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = "ApiRequestError"
    this.status = status
  }
}

export async function requestJson<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  if (init.body && !headers.has("content-type")) {
    headers.set("content-type", "application/json")
  }

  const response = await fetch(path, {
    credentials: "same-origin",
    ...init,
    headers,
  })
  const text = await response.text()
  let data: unknown = null
  if (text) {
    try {
      data = JSON.parse(text)
    } catch (_) {
      data = { error: text }
    }
  }

  if (!response.ok) {
    const body = data as { error?: string } | null
    throw new ApiRequestError(response.status, body?.error ?? response.statusText)
  }
  return data as T
}

function post<T>(path: string, body?: unknown) {
  return requestJson<T>(path, {
    method: "POST",
    body: body === undefined ? undefined : JSON.stringify(body),
  })
}

// ---- auth ----

export interface LoginResult {
  message: string
  /** Installer-provisioned password still active: the UI must collect a new one before the console. */
  must_change_password: boolean
  password_min_length: number
}

export function login(username: string, password: string) {
  return post<LoginResult>("/api/login", { username, password })
}

export function logout() {
  return post<{ message: string }>("/api/logout")
}

// ---- snapshot ----

export function fetchSnapshot() {
  return requestJson<AppSnapshot>("/api/app")
}

// ---- teller ----

/**
 * Resolve teller input — the last 6+ characters of a quote id typed from the
 * customer's wallet, or the full id from a scanner — to the one open quote it
 * identifies. 404 = no match, 409 = ambiguous (type more characters).
 */
export function matchQuote(code: string) {
  return post<Ticket>("/api/quotes/match", { code })
}

export function markPaid(ticketId: string, notes?: string) {
  return post<Ticket>(`/api/tickets/${encodeURIComponent(ticketId)}/mark-paid`, {
    notes: notes ?? "",
  })
}

export function markFailed(ticketId: string, notes?: string) {
  return post<Ticket>(`/api/tickets/${encodeURIComponent(ticketId)}/mark-failed`, {
    notes: notes ?? "",
  })
}

// ---- settled history & circulation ----

export interface HistoryFilters {
  kind?: TicketKind
  status?: "paid" | "failed"
  /** Ticket or quote id fragment, note text, or operator. */
  q?: string
  /** Closed at or after (unix seconds). */
  from?: number
  /** Closed before (unix seconds). */
  to?: number
}

export interface HistoryPage {
  tickets: Ticket[]
  total: number
  offset: number
  limit: number
  /** Paid totals over every row matching the filters. */
  summary: SettledSummary
}

function historyParams(filters: HistoryFilters) {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(filters)) {
    if (value !== undefined && value !== "") params.set(key, String(value))
  }
  return params
}

/** Settled and voided tickets, newest first. Open quotes never appear. */
export function fetchHistory(filters: HistoryFilters, offset: number, limit: number) {
  const params = historyParams(filters)
  params.set("offset", String(offset))
  params.set("limit", String(limit))
  return requestJson<HistoryPage>(`/api/tickets?${params}`)
}

/** Download URL for the same filtered history as CSV. */
export function historyCsvUrl(filters: HistoryFilters) {
  const params = historyParams(filters).toString()
  return params ? `/api/tickets.csv?${params}` : "/api/tickets.csv"
}

/**
 * Daily settled totals ending today, days breaking at the viewer's midnight.
 * `days` omitted = since the first settlement. Empty until anything settled.
 */
export function fetchCirculation(days?: number) {
  const params = new URLSearchParams({ utc_offset: String(-new Date().getTimezoneOffset()) })
  if (days) params.set("days", String(days))
  return requestJson<CirculationDay[]>(`/api/circulation?${params}`)
}

// ---- attachment setup & self-test ----

/**
 * Save the setup/attachment form. Applies live — no restart. The attached
 * mint picks the values up at its next start.
 */
export function saveAttachment(input: {
  unit?: string
  mint_url: string
  advertised_grpc: string
}) {
  return post<{ message: string }>("/api/settings/attachment", input)
}

/**
 * Run the end-to-end self-test: creates one deposit and one payout quote at
 * the mint, verifies both arrive at this processor, then voids them.
 */
export function runSelfTest() {
  return post<SelfTestOutcome>("/api/mint/self-test")
}

// ---- users ----

export function createUser(username: string, password: string, passwordConfirm: string) {
  return post<UserEntry>("/api/users", {
    username,
    password,
    password_confirm: passwordConfirm,
  })
}

export function deleteUser(username: string) {
  return requestJson<{ message: string }>(`/api/users/${encodeURIComponent(username)}`, {
    method: "DELETE",
  })
}

export function changePassword(currentPassword: string, password: string, passwordConfirm: string) {
  return post<{ message: string }>("/api/me/password", {
    current_password: currentPassword,
    password,
    password_confirm: passwordConfirm,
  })
}

export function resetUserPassword(username: string, password: string, passwordConfirm: string) {
  return post<{ message: string }>(`/api/users/${encodeURIComponent(username)}/password`, {
    password,
    password_confirm: passwordConfirm,
  })
}
