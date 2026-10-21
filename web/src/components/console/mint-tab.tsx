import { useState, type FormEvent, type ReactNode } from "react"
import {
  CircleAlert,
  CircleCheck,
  CircleDashed,
  Info,
  RefreshCw,
  TriangleAlert,
  X,
} from "lucide-react"
import { toast } from "sonner"

import {
  runSelfTest,
  saveAttachment,
  type AppSnapshot,
  type CheckStatus,
  type ChecklistItem,
  type SelfTestLeg,
} from "@/lib/api"
import { formatAge } from "@/lib/format"
import { useSnapshot } from "@/lib/snapshot"
import { cn } from "@/lib/utils"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { CopyButton, DetailRow, Disclosure, Field, MonoChip } from "@/components/shared/bits"

/**
 * The Mint tab answers one question first — is the mint attached and working,
 * and if not, what is the one next step — then keeps the checklist detail,
 * the config snippet, and protocol facts one click away.
 */
export function MintTab({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  return (
    <div className="grid gap-4">
      {snapshot.migrated_from_managed && <MigrationNotice />}
      <StatusCard snapshot={snapshot} now={now} />
      <SettingsCard snapshot={snapshot} now={now} />
    </div>
  )
}

function MigrationNotice() {
  return (
    <Alert>
      <Info />
      <AlertTitle>This install previously managed its own mint</AlertTitle>
      <AlertDescription>
        The old configuration — including the bundled mint's recovery phrase — is preserved as
        setup.json.v3-managed.bak in the config directory. The mint itself, its database, keys,
        and backups now belong to whoever operates it.
      </AlertDescription>
    </Alert>
  )
}

// ---------------------------------------------------------------------------
// Status at a glance
// ---------------------------------------------------------------------------

function checkIcon(status: CheckStatus) {
  switch (status) {
    case "ok":
      return <CircleCheck />
    case "warn":
      return <TriangleAlert />
    case "fail":
      return <CircleAlert />
    default:
      return <CircleDashed />
  }
}

const STATUS_WORD: Record<CheckStatus, string> = {
  ok: "OK",
  warn: "Warning",
  fail: "Failing",
  unknown: "Waiting",
}

/** Plain step names for the progress strip, by check id. */
const STEP_LABEL: Record<string, string> = {
  reachable: "Mint online",
  advertised: "Payments offered",
  linked: "Linked",
  keyset: "Keys",
  end_to_end: "Tested",
}

/** Solid = done, outline = needs attention (variant carries meaning), muted = waiting. */
const STEP_BAR: Record<CheckStatus, string> = {
  ok: "bg-primary",
  warn: "border border-foreground",
  fail: "border border-foreground",
  unknown: "bg-muted",
}

/** The check that decides the headline: the first failing, else warning, else waiting one. */
function decidingCheck(checks: ChecklistItem[]) {
  for (const status of ["fail", "warn", "unknown"] as CheckStatus[]) {
    const check = checks.find((item) => item.status === status)
    if (check) return check
  }
  return undefined
}

function headline(
  snapshot: AppSnapshot,
  check: ChecklistItem | undefined,
  now: number,
): { title: string; body: string } {
  const unit = snapshot.setup.unit.toUpperCase()
  if (!snapshot.setup.setup_complete) {
    return {
      title: "Not attached to a mint yet",
      body: "Enter your mint's URL and the unit below. This processor never changes your mint — it checks it and tells you what to fix.",
    }
  }
  if (!check) {
    const test = snapshot.self_test
    return {
      title: "Connected and ready",
      body: `Deposits and payouts flow between your mint and this counter.${
        test ? ` Self-test passed ${formatAge(test.ran_at, now)}.` : ""
      }`,
    }
  }
  const titles: Record<string, string> = {
    "reachable:fail": "Can't reach your mint",
    "advertised:fail": `Your mint doesn't offer ${unit} deposits and payouts yet`,
    "advertised:warn": `${unit} deposits and payouts are only partly set up`,
    "linked:fail": "Your mint hasn't connected to this processor yet",
    "linked:warn": "Your mint started connecting but didn't finish",
    "keyset:fail": `Your mint can't issue ${unit}`,
    "keyset:warn": `Your mint's keys for ${unit} need attention`,
    "end_to_end:fail": "The end-to-end test failed",
    "end_to_end:warn": "Working, with a warning from the self-test",
    "end_to_end:unknown": "Almost ready — the end-to-end test hasn't run yet",
  }
  const title =
    titles[`${check.id}:${check.status}`] ??
    `${check.title}: ${STATUS_WORD[check.status].toLowerCase()}`
  if (check.id === "end_to_end" && check.status === "warn" && snapshot.self_test) {
    return { title, body: snapshot.self_test.warnings.join(" ") }
  }
  if (check.id === "end_to_end" && check.status === "unknown") {
    return {
      title,
      body: "It runs on its own once the mint links up. Run it now to prove a deposit and a payout reach this counter.",
    }
  }
  return { title, body: check.detail }
}

function StatusCard({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const { refresh } = useSnapshot()
  const [testing, setTesting] = useState(false)
  const checks = snapshot.checklist
  const setupComplete = snapshot.setup.setup_complete
  const deciding = setupComplete ? decidingCheck(checks) : undefined
  const head = headline(snapshot, deciding, now)
  const offerTest =
    setupComplete && deciding?.id === "end_to_end" && deciding.status !== "warn"

  async function test() {
    setTesting(true)
    try {
      const outcome = await runSelfTest()
      if (outcome.ok) toast.success("Self-test passed")
      else toast.error("Self-test failed — see the checks for the fix")
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "The self-test could not run.")
    } finally {
      setTesting(false)
      await refresh()
    }
  }

  return (
    // Ends flush on the disclosure row; overflow-hidden keeps its hover inside the radius.
    <Card className={setupComplete ? "overflow-hidden pb-0" : undefined}>
      <CardHeader>
        <CardTitle className="flex items-start gap-2.5 text-base leading-snug">
          <span className="mt-0.5 shrink-0 [&_svg]:size-5">
            {checkIcon(!setupComplete ? "unknown" : (deciding?.status ?? "ok"))}
          </span>
          {head.title}
        </CardTitle>
        <CardDescription className="pl-[30px] wrap-anywhere">{head.body}</CardDescription>
        {setupComplete && (
          <CardAction>
            <Button
              variant="ghost"
              size="icon"
              aria-label="Check again"
              onClick={() => void refresh()}
            >
              <RefreshCw />
            </Button>
          </CardAction>
        )}
      </CardHeader>

      {setupComplete && (
        <CardContent className="grid gap-5">
          <ol
            aria-label="Attachment checks"
            className="m-0 grid list-none gap-2 p-0 sm:grid-cols-5 sm:gap-3"
          >
            {checks.map((check) => (
              <li
                key={check.id}
                className="flex items-center gap-2 sm:grid sm:items-start sm:gap-2"
              >
                <span
                  aria-hidden
                  className={cn("hidden h-1.5 rounded-full sm:block", STEP_BAR[check.status])}
                />
                <span
                  className={cn(
                    "flex min-w-0 items-center gap-1.5 text-sm [&_svg]:size-4 [&_svg]:shrink-0",
                    check.status === "fail" || check.status === "warn"
                      ? "font-semibold"
                      : "font-medium",
                  )}
                >
                  <span
                    className={
                      check.status === "ok" || check.status === "unknown"
                        ? "text-muted-foreground"
                        : undefined
                    }
                  >
                    {checkIcon(check.status)}
                  </span>
                  <span className="truncate">{STEP_LABEL[check.id] ?? check.title}</span>
                </span>
                <span
                  className={cn(
                    "ml-auto text-xs sm:ml-0 sm:pl-[22px]",
                    check.status === "fail" ? "font-medium text-foreground" : "text-muted-foreground",
                  )}
                >
                  {STATUS_WORD[check.status]}
                </span>
              </li>
            ))}
          </ol>

          {deciding?.remedy && (
            <div className="rounded-md border bg-muted p-3 text-sm leading-6 wrap-anywhere">
              <span className="font-semibold">Next step. </span>
              {deciding.remedy}
            </div>
          )}
          {offerTest && (
            <div>
              <Button loading={testing} onClick={() => void test()}>
                Run self-test
              </Button>
            </div>
          )}
        </CardContent>
      )}

      {setupComplete && (
        <CardContent className="border-t px-0">
          <Disclosure bleed summary="All checks and the self-test" meta={`${checks.length} checks`}>
            <div className="grid gap-4">
              {checks.map((check) => (
                <ChecklistRow key={check.id} check={check} />
              ))}
              <SelfTestDetails snapshot={snapshot} now={now} testing={testing} onTest={test} />
            </div>
          </Disclosure>
        </CardContent>
      )}
    </Card>
  )
}

function CheckBadge({ status }: { status: CheckStatus }) {
  if (status === "ok") return <Badge variant="solid">OK</Badge>
  if (status === "warn") return <Badge variant="outline">Warning</Badge>
  if (status === "fail")
    return (
      <Badge variant="outline">
        <X />
        Failing
      </Badge>
    )
  return <Badge variant="muted">Waiting</Badge>
}

function ChecklistRow({ check }: { check: ChecklistItem }) {
  return (
    <div className="flex items-start gap-2.5">
      <span className="mt-0.5 text-muted-foreground [&_svg]:size-4">{checkIcon(check.status)}</span>
      <div className="min-w-0 flex-1 grid gap-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium">{check.title}</span>
          <CheckBadge status={check.status} />
        </div>
        <p className="m-0 text-sm text-muted-foreground wrap-anywhere">{check.detail}</p>
        {check.remedy && (
          <div className="mt-1 rounded-md border bg-muted p-2.5 text-sm leading-6 wrap-anywhere">
            {check.remedy}
          </div>
        )}
      </div>
    </div>
  )
}

function LegLine({ leg }: { leg: SelfTestLeg }) {
  return (
    <div className="flex items-start gap-2.5">
      <span className="mt-0.5 text-muted-foreground [&_svg]:size-4">
        {leg.ok ? <CircleCheck /> : <CircleAlert />}
      </span>
      <div className="min-w-0 text-sm wrap-anywhere">
        {leg.detail}
        {leg.remedy && <p className="m-0 mt-1 text-muted-foreground">{leg.remedy}</p>}
      </div>
    </div>
  )
}

function formatTtl(secs: number) {
  return secs >= 120 ? `${Math.round(secs / 60)} min` : `${secs} s`
}

function SelfTestDetails({
  snapshot,
  now,
  testing,
  onTest,
}: {
  snapshot: AppSnapshot
  now: number
  testing: boolean
  onTest: () => Promise<void>
}) {
  const result = snapshot.self_test
  const unitLabel = snapshot.setup.unit ? snapshot.setup.unit.toUpperCase() : "unit"
  return (
    <div className="grid gap-3 border-t pt-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="grid min-w-0 flex-1 gap-1">
          <span className="text-sm font-medium">Self-test</span>
          <p className="m-0 max-w-[65ch] text-sm text-muted-foreground">
            Creates one deposit and one payout quote at the mint (1 {unitLabel} each), checks both
            reach this processor, then voids them. Nothing is paid — safe to run anytime.
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          loading={testing}
          disabled={!snapshot.setup.setup_complete}
          onClick={() => void onTest()}
        >
          Run self-test
        </Button>
      </div>
      {result && (
        <>
          <div className="flex flex-wrap items-center gap-2">
            {result.ok ? (
              <Badge variant="solid">Passed</Badge>
            ) : (
              <Badge variant="outline">
                <X />
                Failed
              </Badge>
            )}
            <span className="text-sm text-muted-foreground">
              {formatAge(result.ran_at, now)}
              {result.latency_ms != null && ` · ${result.latency_ms} ms round trip`}
            </span>
          </div>
          <LegLine leg={result.deposit} />
          <LegLine leg={result.payout} />
          {result.warnings.map((warning) => (
            <div key={warning} className="flex items-start gap-2.5 text-sm">
              <TriangleAlert className="mt-0.5 size-4 shrink-0" />
              <span>{warning}</span>
            </div>
          ))}
        </>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Settings, config snippet, technical details
// ---------------------------------------------------------------------------

function SettingsCard({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const { refresh } = useSnapshot()
  const setup = snapshot.setup
  const [editing, setEditing] = useState(!setup.setup_complete)
  const [unit, setUnit] = useState(setup.unit)
  const [mintUrl, setMintUrl] = useState(setup.mint_url)
  const [grpc, setGrpc] = useState(setup.advertised_grpc || defaultGrpc())
  const [busy, setBusy] = useState(false)

  function defaultGrpc() {
    return `${window.location.hostname}:${setup.published_grpc_port}`
  }

  function startEditing() {
    setUnit(setup.unit)
    setMintUrl(setup.mint_url)
    setGrpc(setup.advertised_grpc || defaultGrpc())
    setEditing(true)
  }

  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    try {
      await saveAttachment({
        unit: setup.unit_change_allowed ? unit : undefined,
        mint_url: mintUrl,
        advertised_grpc: grpc,
      })
      toast.success("Attachment saved — no restart needed")
      setEditing(false)
      await refresh()
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not save the attachment.")
    } finally {
      setBusy(false)
    }
  }

  const statusOf = (id: string) => snapshot.checklist.find((check) => check.id === id)?.status
  // The snippet matters until the mint advertises the unit and has linked up.
  const snippetNeeded = statusOf("advertised") !== "ok" || statusOf("linked") !== "ok"

  return (
    <Card className="overflow-hidden pb-0">
      <CardHeader>
        <CardTitle>Connection settings</CardTitle>
        <CardDescription>
          Which mint this processor serves, and the one unit it settles. Changes apply live.
        </CardDescription>
        {!editing && (
          <CardAction>
            <Button variant="outline" size="sm" onClick={startEditing}>
              Edit
            </Button>
          </CardAction>
        )}
      </CardHeader>
      <CardContent>
        {editing ? (
          <form className="grid gap-4" onSubmit={submit}>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field
                label="Mint URL"
                htmlFor="attach-mint-url"
                help="The mint's public address — the same URL wallets use."
              >
                <Input
                  id="attach-mint-url"
                  type="url"
                  value={mintUrl}
                  onChange={(event) => setMintUrl(event.target.value)}
                  placeholder="https://mint.example.org"
                  required
                  autoFocus={!setup.setup_complete}
                />
              </Field>
              <Field
                label="Unit"
                htmlFor="attach-unit"
                help={
                  setup.unit_change_allowed
                    ? "Lowercase letters, digits, - and _. Locks after the first successful self-test."
                    : "Locked — issued ecash and quotes reference this unit."
                }
              >
                <Input
                  id="attach-unit"
                  className="font-mono"
                  value={unit}
                  onChange={(event) => setUnit(event.target.value.toLowerCase())}
                  disabled={!setup.unit_change_allowed}
                  placeholder="ora"
                  required
                />
              </Field>
            </div>
            <Disclosure
              summary="Advanced"
              meta="how your mint reaches this processor"
              defaultOpen={!grpc}
            >
              <div className="sm:max-w-[calc(50%-0.375rem)]">
                <Field
                  label="Processor address, as seen by the mint"
                  htmlFor="attach-grpc"
                  help="host or host:port of this processor's gRPC endpoint; goes into the config snippet."
                >
                  <Input
                    id="attach-grpc"
                    className="font-mono"
                    value={grpc}
                    onChange={(event) => setGrpc(event.target.value)}
                    placeholder="10.0.0.5:50051"
                    required
                  />
                </Field>
              </div>
            </Disclosure>
            <div className="flex gap-2">
              {setup.setup_complete && (
                <Button type="button" variant="outline" onClick={() => setEditing(false)}>
                  Cancel
                </Button>
              )}
              <Button type="submit" loading={busy}>
                Save
              </Button>
            </div>
          </form>
        ) : (
          <div className="grid gap-2.5">
            <DetailRow label="Mint">
              <span className="inline-flex flex-wrap items-center gap-2">
                {snapshot.mint_identity?.name && (
                  <span className="text-sm">{snapshot.mint_identity.name}</span>
                )}
                <MonoChip>{setup.mint_url || "not set"}</MonoChip>
              </span>
            </DetailRow>
            <DetailRow label="Unit">
              <span className="inline-flex items-center gap-2">
                <MonoChip>{setup.unit || "not set"}</MonoChip>
                {setup.unit_locked && <Badge variant="muted">Locked</Badge>}
              </span>
            </DetailRow>
          </div>
        )}
      </CardContent>

      <CardContent className="divide-y border-t px-0">
        {snapshot.snippet ? (
          <Disclosure
            bleed
            summary="Config for your cdk-mintd"
            meta={snippetNeeded ? "apply this to attach" : "in use by the mint"}
            defaultOpen={snippetNeeded}
          >
            <SnippetBody snippet={snapshot.snippet} />
          </Disclosure>
        ) : (
          <p className="m-0 px-6 py-3 text-sm text-muted-foreground">
            The config snippet for your mintd appears once the settings above are saved.
          </p>
        )}
        <Disclosure bleed summary="Technical details">
          <TechnicalDetails snapshot={snapshot} now={now} />
        </Disclosure>
      </CardContent>
    </Card>
  )
}

function SnippetBody({ snippet }: { snippet: string }) {
  return (
    <div className="grid gap-3">
      <div className="flex items-start justify-between gap-3">
        <p className="m-0 max-w-[65ch] text-sm text-muted-foreground">
          Merge this into your mintd&apos;s stored configuration with cdk-mintd config apply (the
          header lists the exact commands), then restart the mint. The checks above settle on
          their own once it reconnects.
        </p>
        <CopyButton value={snippet} label="Copy mint.toml snippet" />
      </div>
      <pre className="m-0 max-h-72 overflow-auto rounded-md border bg-muted p-3 font-mono text-xs leading-5">
        {snippet}
      </pre>
    </div>
  )
}

function TechnicalDetails({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const setup = snapshot.setup
  const identity = snapshot.mint_identity
  const signals = snapshot.attach_signals
  const test = snapshot.self_test
  const rows: Array<[string, ReactNode]> = [
    ["Payment method", <MonoChip key="method">{setup.method}</MonoChip>],
    [
      "Processor address, as seen by the mint",
      <MonoChip key="grpc">{setup.advertised_grpc || "not set"}</MonoChip>,
    ],
    [
      "Listening on",
      <span key="bind" className="inline-flex flex-wrap items-center gap-2">
        <MonoChip>{setup.grpc_bind}</MonoChip>
        <span className="text-sm text-muted-foreground">
          {setup.grpc_tls ? "TLS on" : "no TLS — private network only"}
        </span>
      </span>,
    ],
    [
      "Mint software",
      <MonoChip key="version">{identity?.version || "unknown"}</MonoChip>,
    ],
    [
      "Mint settings read",
      <span key="settings" className="text-sm">
        {signals.last_settings_at ? formatAge(signals.last_settings_at, now) : "Not since this processor started"}
      </span>,
    ],
    [
      "Payment stream",
      <span key="stream" className="text-sm">
        {signals.stream_attached_at
          ? `Attached ${formatAge(signals.stream_attached_at, now)}`
          : "Not attached"}
      </span>,
    ],
  ]
  if (test?.mint_quote_ttl_secs != null || test?.melt_quote_ttl_secs != null) {
    rows.push([
      "Quote lifetimes at the mint",
      <span key="ttl" className="text-sm">
        {[
          test.mint_quote_ttl_secs != null && `deposits ${formatTtl(test.mint_quote_ttl_secs)}`,
          test.melt_quote_ttl_secs != null && `payouts ${formatTtl(test.melt_quote_ttl_secs)}`,
        ]
          .filter(Boolean)
          .join(" · ")}
      </span>,
    ])
  }
  if (identity?.description) {
    rows.push(["Mint description", <span key="desc" className="text-sm">{identity.description}</span>])
  }
  return (
    <div className="grid gap-2.5">
      {rows.map(([label, value]) => (
        <DetailRow key={label} label={label}>
          {value}
        </DetailRow>
      ))}
      <p className="m-0 mt-1 text-xs text-muted-foreground">
        Mint name, description, and version come from your mint&apos;s [mint_info] — edit them
        there.
      </p>
    </div>
  )
}
