import { useCallback, useEffect, useRef, useState, type FormEvent } from "react"
import { toast } from "sonner"
import { createCustomer, getCustomers, updateCustomer, type CustomerPage, type WalletCustomer } from "@/lib/api"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { Field } from "@/components/shared/bits"

type Edit = { kind: "create" } | { kind: "reset" | "disable" | "enable" | "sign_out"; customer: WalletCustomer }
const titles = { create: "Add customer", reset: "Reset password", disable: "Disable access", enable: "Enable access", sign_out: "Sign out" }
const failure = (error: unknown) => error instanceof Error ? error.message : "Could not reach Pecan. Try again."
const tokenLimit = "Existing access tokens remain valid until expiry. Already issued blind tokens remain usable; these actions do not revoke them or change ecash balances."

export function WalletCustomers() {
  const [page, setPage] = useState<CustomerPage | null>(null)
  const [search, setSearch] = useState("")
  const [query, setQuery] = useState({ search: "", offset: 0 })
  const [busy, setBusy] = useState(true)
  const [error, setError] = useState("")
  const [edit, setEdit] = useState<Edit | null>(null)
  const request = useRef(0)
  const refresh = useCallback(async () => {
    const id = ++request.current
    setBusy(true)
    try {
      const data = await getCustomers(query.search, query.offset)
      if (id === request.current) { setPage(data); setError("") }
    } catch (err) {
      if (id === request.current) { setPage(null); setError(failure(err)) }
    } finally { if (id === request.current) setBusy(false) }
  }, [query])
  useEffect(() => { void refresh(); return () => { request.current++ } }, [refresh])

  return <Card>
    <CardHeader>
      <CardTitle>Wallet customers</CardTitle>
      <CardDescription className="col-span-full">Manage customer sign-in through Keycloak. These accounts give access to the mint; Pecan staff accounts are managed below.</CardDescription>
      {page?.configured && <CardAction className="row-span-1"><Button size="sm" disabled={busy} onClick={() => setEdit({ kind: "create" })}>Add customer</Button></CardAction>}
    </CardHeader>
    <CardContent className="grid gap-4" aria-busy={busy}>
      {error && <Alert><AlertTitle>Customer management unavailable</AlertTitle><AlertDescription>{error}</AlertDescription><Button className="mt-3" size="sm" variant="outline" loading={busy} onClick={() => void refresh()}>Retry</Button></Alert>}
      {!page && busy && <div className="grid gap-3" role="status" aria-label="Loading customers"><Skeleton className="h-9 w-full" /><Skeleton className="h-24 w-full" /></div>}
      {page && !page.configured && <div className="grid gap-3">
        <p className="text-sm">Connect a Keycloak customer realm to add accounts and manage access here.</p>
        <p className="max-w-prose text-sm text-muted-foreground">Set <code>KEYCLOAK_ISSUER</code>, <code>KEYCLOAK_CLIENT_ID</code> and <code>KEYCLOAK_CLIENT_SECRET</code> in the installation’s <code>.env</code>, then recreate the processor. Use a separate management client with customer-management permissions. The realm must match the mint’s active identity provider.</p>
        <a className="w-fit text-sm underline underline-offset-4" href="https://github.com/zeugmaster/pecan/blob/main/docs/operations.md#wallet-customer-management-keycloak" target="_blank" rel="noopener noreferrer">Keycloak setup instructions</a>
        <Button className="w-fit" variant="outline" size="sm" loading={busy} onClick={() => void refresh()}>Check connection</Button>
      </div>}
      {page?.configured && <>
        <p className="break-all text-xs text-muted-foreground">Customer realm · {page.issuer}</p>
        <form className="flex flex-wrap items-end gap-2" onSubmit={event => { event.preventDefault(); setQuery({ search: search.trim(), offset: 0 }) }}>
          <div className="min-w-0 flex-1"><Field label="Find a customer" htmlFor="customer-search"><Input id="customer-search" type="search" value={search} maxLength={200} onChange={event => setSearch(event.target.value)} placeholder="Username or email" /></Field></div>
          <Button type="submit" variant="outline" loading={busy}>Search</Button>
          <Button type="button" variant="ghost" aria-label="Refresh customers" disabled={busy} onClick={() => void refresh()}>Refresh</Button>
        </form>
        <div className="overflow-x-auto rounded-md border">
          <Table className="table-fixed sm:table-auto">
            <TableHeader><TableRow><TableHead className="w-[45%] sm:w-auto">Customer</TableHead><TableHead className="hidden sm:table-cell">Status</TableHead><TableHead className="text-right">Actions</TableHead></TableRow></TableHeader>
            <TableBody>
              {page.customers.map(customer => <TableRow key={customer.id}>
                <TableCell className="max-w-64 whitespace-normal align-top"><span className="break-all font-medium">{customer.username}</span>{customer.email && <span className="mt-1 block break-all text-xs text-muted-foreground">{customer.email}</span>}<Badge className="mt-2 sm:hidden">{customer.enabled ? "Enabled" : "Disabled"}</Badge>{customer.requiredActions.includes("UPDATE_PASSWORD") && <span className="mt-1 block text-xs text-muted-foreground">Password change required</span>}</TableCell>
                <TableCell className="hidden align-top sm:table-cell"><Badge>{customer.enabled ? "Enabled" : "Disabled"}</Badge></TableCell>
                <TableCell className="align-top"><div className="flex flex-col items-end gap-1 sm:flex-row sm:flex-wrap sm:justify-end">
                  <Button size="sm" variant="ghost" disabled={busy} aria-label={`Reset password for ${customer.username}`} onClick={() => setEdit({ kind: "reset", customer })}>Reset password</Button>
                  <Button size="sm" variant="ghost" disabled={busy} aria-label={`Sign out ${customer.username}`} onClick={() => setEdit({ kind: "sign_out", customer })}>Sign out</Button>
                  <Button size="sm" variant="outline" disabled={busy} aria-label={`${customer.enabled ? "Disable" : "Enable"} ${customer.username}`} onClick={() => setEdit({ kind: customer.enabled ? "disable" : "enable", customer })}>{customer.enabled ? "Disable" : "Enable"}</Button>
                </div></TableCell>
              </TableRow>)}
              {!page.customers.length && <TableRow><TableCell colSpan={3} className="py-8 text-center text-muted-foreground">{query.search ? "No customers match this search." : query.offset ? "No more customers. Go back to the previous page." : "No customers yet. Add a customer to give them a wallet sign-in."}</TableCell></TableRow>}
            </TableBody>
          </Table>
        </div>
        <div className="flex items-center justify-between gap-3">
          <p className="text-xs text-muted-foreground" role="status">{page.customers.length} shown · page {Math.floor(query.offset / page.page_size) + 1}</p>
          <div className="flex gap-2">
            <Button size="sm" variant="outline" disabled={busy || !query.offset} onClick={() => setQuery(q => ({ ...q, offset: Math.max(0, q.offset - page.page_size) }))}>Previous</Button>
            <Button size="sm" variant="outline" disabled={busy || !page.has_more} onClick={() => setQuery(q => ({ ...q, offset: q.offset + page.page_size }))}>Next</Button>
          </div>
        </div>
        <p className="max-w-prose text-sm text-muted-foreground">{tokenLimit}</p>
      </>}
    </CardContent>
    {edit && <CustomerDialog edit={edit} close={() => setEdit(null)} refresh={refresh} />}
  </Card>
}

function CustomerDialog({ edit, close, refresh }: { edit: Edit; close: () => void; refresh: () => Promise<void> }) {
  const returnFocus = useRef(document.activeElement)
  const [username, setUsername] = useState("")
  const [email, setEmail] = useState("")
  const [password, setPassword] = useState("")
  const [confirm, setConfirm] = useState("")
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const passwordNeeded = edit.kind === "create" || edit.kind === "reset"
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (passwordNeeded && password !== confirm) { setError("Passwords do not match."); return }
    setBusy(true); setError("")
    try {
      if (edit.kind === "create") await createCustomer({ username, email, password, password_confirm: confirm })
      else if (edit.kind === "reset") await updateCustomer(edit.customer.id, { action: "reset_password", password, password_confirm: confirm })
      else if (edit.kind === "sign_out") await updateCustomer(edit.customer.id, { action: "sign_out" })
      else await updateCustomer(edit.customer.id, { action: "set_enabled", enabled: edit.kind === "enable" })
      toast.success(edit.kind === "create" ? "Customer added" : edit.kind === "reset" ? "Temporary password set; customer signed out" : edit.kind === "disable" ? "Customer disabled and signed out" : edit.kind === "enable" ? "Customer enabled" : "Customer signed out")
      await refresh()
      close()
    } catch (err) { setError(failure(err)); await refresh() }
    finally { setBusy(false) }
  }

  return <Dialog open onOpenChange={open => { if (!open && !busy) close() }}>
    <DialogContent className="max-h-[90dvh] overflow-y-auto" onCloseAutoFocus={event => {
      event.preventDefault()
      if (returnFocus.current instanceof HTMLElement && returnFocus.current.isConnected) returnFocus.current.focus()
      else document.getElementById("customer-search")?.focus()
    }}>
      <form className="grid gap-4" onSubmit={submit}>
        <DialogHeader>
          <DialogTitle>{titles[edit.kind]}{edit.kind !== "create" && <span className="block break-all text-base font-normal">{edit.customer.username}</span>}</DialogTitle>
          <DialogDescription>{passwordNeeded ? "Give the temporary password to the customer directly. They must choose their own password on Keycloak’s sign-in page. No email is sent." : edit.kind === "disable" ? "Block new sign-ins and session refresh, and sign this customer out." : edit.kind === "enable" ? "Allow this customer to sign in to the mint again." : "End current sessions and revoke the wallet’s offline refresh tokens. This customer can sign in again."}</DialogDescription>
        </DialogHeader>
        {edit.kind === "create" && <>
          <Field label="Username" htmlFor="customer-name"><Input id="customer-name" value={username} onChange={event => setUsername(event.target.value)} maxLength={255} autoComplete="off" required autoFocus disabled={busy} /></Field>
          <Field label="Email (optional)" htmlFor="customer-email"><Input id="customer-email" type="email" value={email} onChange={event => setEmail(event.target.value)} maxLength={254} autoComplete="off" disabled={busy} /></Field>
        </>}
        {passwordNeeded && <>
          <Field label="Temporary password" htmlFor="customer-password" help="At least 8 characters. The Keycloak realm’s password policy also applies."><Input id="customer-password" type="password" value={password} onChange={event => setPassword(event.target.value)} minLength={8} maxLength={1024} autoComplete="new-password" required disabled={busy} /></Field>
          <Field label="Confirm temporary password" htmlFor="customer-confirm"><Input id="customer-confirm" type="password" value={confirm} onChange={event => setConfirm(event.target.value)} minLength={8} maxLength={1024} autoComplete="new-password" required disabled={busy} /></Field>
        </>}
        {edit.kind !== "create" && edit.kind !== "enable" && <p className="text-sm text-muted-foreground">{tokenLimit}</p>}
        {error && <p role="alert" className="text-sm">{error}</p>}
        <DialogFooter><Button type="button" variant="outline" disabled={busy} onClick={close}>Cancel</Button><Button type="submit" loading={busy}>{titles[edit.kind]}</Button></DialogFooter>
      </form>
    </DialogContent>
  </Dialog>
}
