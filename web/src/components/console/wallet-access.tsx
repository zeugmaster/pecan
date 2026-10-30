import { useCallback, useEffect, useState, type FormEvent } from "react"
import { toast } from "sonner"
import { getMintAuth, saveMintAuth, startMintAuthLogin, pollMintAuthLogin, disconnectMintAuth, type AuthDraft, type AuthMode, type MintAuthLogin, type MintAuthStatus } from "@/lib/api"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { CopyButton, DetailRow, Disclosure, Field } from "@/components/shared/bits"

const operations = [
  ["get_mint_quote", "Request a deposit quote"],
  ["check_mint_quote", "Check a deposit quote"],
  ["mint", "Issue ecash"],
  ["get_melt_quote", "Request a withdrawal quote"],
  ["check_melt_quote", "Check a withdrawal quote"],
  ["melt", "Redeem ecash"],
  ["swap", "Receive / swap ecash"],
  ["restore", "Restore wallet outputs"],
  ["check_proof_state", "Check spent proofs"],
  ["websocket_auth", "Subscribe to updates"],
] as const
const labels = { none: "Open", clear: "Clear auth", blind: "Blind auth" }
const errorText = (error: unknown) => error instanceof Error ? error.message : "Could not reach Pecan. Try again."

function activeDraft(status: MintAuthStatus): AuthDraft {
  return {
    auth_enabled: !!(status.active?.clear || status.active?.blind),
    openid_discovery: status.active?.clear?.openid_discovery ?? "",
    openid_client_id: status.active?.clear?.client_id ?? "",
    mint_max_bat: status.active?.blind?.bat_max_mint ?? 32,
    operations: Object.fromEntries(operations.map(([key]) => [key, status.operations?.[key] ?? "none"])),
  }
}

export function WalletAccess({ mintUrl }: { mintUrl: string }) {
  const [status, setStatus] = useState<MintAuthStatus | null>(null)
  const [error, setError] = useState("")
  const [refreshing, setRefreshing] = useState(false)
  const refresh = useCallback(async () => {
    setRefreshing(true)
    try { setStatus(await getMintAuth()); setError("") }
    catch (err) { setError(errorText(err)) }
    finally { setRefreshing(false) }
  }, [])
  useEffect(() => { void refresh() }, [refresh, mintUrl])

  return <Card>
    <CardHeader>
      <CardTitle>Wallet access</CardTitle>
      <CardDescription>Control which mint operations require a customer to authenticate. Pecan staff accounts are separate.</CardDescription>
    </CardHeader>
    <CardContent className="grid gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Active at the mint</span>
          <Badge>{status?.error || error ? "Unknown" : !status ? "Loading" : status.active?.clear || status.active?.blind ? "Authentication enabled" : "Open access"}</Badge>
        </div>
        <Button variant="outline" size="sm" loading={refreshing} onClick={() => void refresh()}>Refresh policy</Button>
      </div>
      {(error || status?.error) && <Alert><AlertTitle>Policy could not be verified</AlertTitle><AlertDescription>{error || status?.error} The saved draft does not indicate what is active.</AlertDescription></Alert>}
      {status && <>
        <ProcessorConnection status={status} refresh={refresh} />
        <PolicyForm key={mintUrl} status={status} refresh={refresh} />
      </>}
    </CardContent>
  </Card>
}

function ProcessorConnection({ status, refresh }: { status: MintAuthStatus; refresh: () => Promise<void> }) {
  const [login, setLogin] = useState<MintAuthLogin | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  useEffect(() => {
    if (!login) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout>
    async function poll() {
      try {
        if (Date.now() >= login!.expires_at * 1000) throw new Error("Sign-in expired. Start again.")
        const result = await pollMintAuthLogin(login!.id)
        if (cancelled) return
        if (result.status === "connected") {
          setLogin(null)
          toast.success("Processor signed in to the mint")
          await refresh()
        } else timer = setTimeout(() => void poll(), (result.interval ?? login!.interval) * 1000)
      } catch (err) {
        if (!cancelled) { setError(errorText(err)); setLogin(null) }
      }
    }
    timer = setTimeout(() => void poll(), login.interval * 1000)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [login, refresh])

  async function connect() {
    setBusy(true); setError("")
    try { setLogin(await startMintAuthLogin()) }
    catch (err) { setError(errorText(err)) }
    finally { setBusy(false) }
  }
  async function disconnect() {
    setBusy(true); setError("")
    try { await disconnectMintAuth(); setLogin(null); await refresh() }
    catch (err) { setError(errorText(err)) }
    finally { setBusy(false) }
  }
  return <section className="grid gap-3 rounded-lg border p-4" aria-label="Processor sign-in">
    <DetailRow label="Processor sign-in">{status.connection.connected ? "Connected" : "Not connected"}</DetailRow>
    <p className="text-sm text-muted-foreground">Pecan needs its own sign-in when quote creation or lookup is protected. Customer wallets sign in independently. Use an account allowed by your mint’s identity provider.</p>
    {status.connection.connected && <p className="text-sm text-muted-foreground">{status.connection.blind_tokens} one-use blind tokens available. {status.connection.can_refresh ? "Session refresh is available." : "Sign in again when the session expires."} Tokens refill automatically when needed.</p>}
    {login ? <div className="grid gap-3" role="status">
      <p className="text-sm">Open the provider and enter this code. Pecan will continue when you approve.</p>
      <div className="flex items-center gap-3"><code className="text-xl tracking-widest">{login.user_code}</code><CopyButton value={login.user_code} label="Copy sign-in code" /></div>
      <a className="break-all text-sm underline underline-offset-4" href={login.verification_uri} target="_blank" rel="noopener noreferrer">{login.verification_uri}</a>
      <p className="text-xs text-muted-foreground">Waiting for approval · expires at {new Date(login.expires_at * 1000).toLocaleTimeString()}</p>
      <Button className="w-fit" variant="outline" loading={busy} onClick={() => void disconnect()}>Cancel sign-in</Button>
    </div> : <div className="flex flex-wrap gap-2">
      <Button variant="outline" loading={busy} disabled={!status.active?.clear || !!status.error} onClick={() => void connect()}>{status.connection.connected ? "Sign in again" : "Connect processor"}</Button>
      {status.connection.stored && <Button variant="ghost" disabled={busy} onClick={() => void disconnect()}>Disconnect processor</Button>}
    </div>}
    {error && <p role="alert" className="text-sm">{error}</p>}
  </section>
}

function PolicyForm({ status, refresh }: { status: MintAuthStatus; refresh: () => Promise<void> }) {
  const [draft, setDraft] = useState<AuthDraft>(() => status.draft ?? activeDraft(status))
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const update = (patch: Partial<AuthDraft>) => setDraft(current => ({ ...current, ...patch }))
  const applied = status.active && status.draft && (() => {
    const active = activeDraft(status)
    if (!status.draft.auth_enabled) return !active.auth_enabled
    return active.auth_enabled && active.openid_discovery === status.draft.openid_discovery && active.openid_client_id === status.draft.openid_client_id && active.mint_max_bat === status.draft.mint_max_bat && operations.every(([key]) => active.operations[key] === status.draft!.operations[key])
  })()
  async function save(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError("")
    try { await saveMintAuth(draft); await refresh(); toast.success("Configuration draft saved") }
    catch (err) { setError(errorText(err)) }
    finally { setBusy(false) }
  }
  return <form onSubmit={save} className="grid gap-5">
    <div>
      <h3 className="text-sm font-semibold">Configure wallet access</h3>
      <p className="mt-1 text-sm text-muted-foreground">Clear auth identifies the account on each request. Blind auth spends an unlinkable, one-use authorization token; the wallet signs in to obtain tokens first. These tokens have no monetary value.</p>
    </div>
    <div className="flex items-center gap-3"><Switch id="wallet-auth-enabled" checked={draft.auth_enabled} onCheckedChange={auth_enabled => update({ auth_enabled })} /><label htmlFor="wallet-auth-enabled" className="text-sm font-medium">Require authentication</label></div>
    {draft.auth_enabled && operations.every(([key]) => draft.operations[key] === "none") && <p className="text-sm text-muted-foreground">All operations in this draft are open. Choose which operations to protect below.</p>}
    <div className="grid gap-4 sm:grid-cols-2">
      <Field label="OpenID discovery URL" htmlFor="auth-discovery" help="HTTPS discovery document for the mint’s identity provider.">
        <Input id="auth-discovery" type="url" value={draft.openid_discovery} required={draft.auth_enabled} disabled={!draft.auth_enabled} onChange={e => update({ openid_discovery: e.target.value })} placeholder="https://id.example/.well-known/openid-configuration" />
      </Field>
      <Field label="OpenID client ID" htmlFor="auth-client" help="Use a public client with device authorization enabled for Pecan.">
        <Input id="auth-client" value={draft.openid_client_id} required={draft.auth_enabled} disabled={!draft.auth_enabled} onChange={e => update({ openid_client_id: e.target.value })} />
      </Field>
      <Field label="Maximum blind tokens per request" htmlFor="auth-limit" help="A batch limit, not a per-person quota or an amount limit.">
        <Input id="auth-limit" type="number" min={1} max={10000} required disabled={!draft.auth_enabled} value={draft.mint_max_bat} onChange={e => update({ mint_max_bat: e.target.valueAsNumber })} />
      </Field>
    </div>
    <div className="overflow-x-auto rounded-md border">
      <Table className="table-fixed sm:table-auto">
        <TableHeader><TableRow><TableHead className="w-[48%] sm:w-auto">Operation</TableHead><TableHead className="hidden sm:table-cell">Active · branch</TableHead><TableHead className="whitespace-normal">Draft · all methods</TableHead></TableRow></TableHeader>
        <TableBody>{operations.map(([key, label]) => <TableRow key={key}>
          <TableCell className="whitespace-normal font-medium">{label}<span className="mt-1 block text-xs font-normal text-muted-foreground sm:hidden">Active: {status.operations ? labels[status.operations[key]] : "Unknown"}</span></TableCell>
          <TableCell className="hidden sm:table-cell">{status.operations ? labels[status.operations[key]] : "Unknown"}</TableCell>
          <TableCell><select aria-label={`${label} authentication`} className="h-9 w-full min-w-28 rounded-md border border-input bg-background px-2 text-sm focus-visible:outline-ring" value={draft.operations[key]} disabled={!draft.auth_enabled} onChange={e => update({ operations: { ...draft.operations, [key]: e.target.value as AuthMode } })}>
            {Object.entries(labels).map(([value, text]) => <option key={value} value={value}>{text}</option>)}
          </select></TableCell>
        </TableRow>)}</TableBody>
      </Table>
    </div>
    <p className="text-xs leading-5 text-muted-foreground">CDK applies these settings to every payment method on the mint. Protecting swaps also affects receiving ecash. Blind auth hides the link between token issuance and use; it does not hide network metadata. Existing blind tokens remain usable after an identity-provider account is disabled.</p>
    {error && <p role="alert" className="text-sm">{error}</p>}
    <div className="flex flex-wrap gap-2">
      <Button type="submit" loading={busy}>Save configuration draft</Button>
      <Button type="button" variant="outline" disabled={!status.active} onClick={() => setDraft(activeDraft(status))}>Use active policy</Button>
    </div>
    {status.snippet && <section className="grid gap-3 rounded-lg border p-4" aria-label="Saved auth configuration">
      <div className="flex items-center justify-between gap-3"><span className="text-sm font-medium">Saved draft · {applied ? "matches active policy" : "not confirmed active"}</span><CopyButton value={status.snippet} label="Copy saved auth configuration" /></div>
      <p className="text-sm text-muted-foreground">Saving changes Pecan’s draft only. Export your mint’s full configuration, replace its [auth] section with this block, review and apply the full document, then restart mintd. Preserve the existing payment backends, keys and RPC settings. Refresh policy here to verify, connect the processor, and rerun the Mint self-test.</p>
      <Disclosure summary="Saved mint configuration"><pre className="overflow-x-auto text-xs leading-6">{status.snippet}</pre></Disclosure>
    </section>}
    {status.active && <Disclosure summary="Advertised protected endpoints"><pre className="overflow-x-auto text-xs leading-6">{JSON.stringify(status.active, null, 2)}</pre></Disclosure>}
  </form>
}
