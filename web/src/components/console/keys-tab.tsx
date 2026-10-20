import { useState } from "react"
import { TriangleAlert } from "lucide-react"

import type { AppSnapshot, KeysetEntry } from "@/lib/api"
import { formatDate, formatRelative } from "@/lib/format"
import { isKeysetExpired, keysetState, plannedExpiry, sortKeysets } from "@/lib/keysets"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { KeysetBadge } from "@/components/shared/badges"
import {
  CopyButton,
  DetailRow,
  EmptyState,
  MonoChip,
  SegmentedControl,
} from "@/components/shared/bits"
import { KeyTimeline, KeyTimelineLegend } from "@/components/console/key-timeline"

const LIFETIMES = [
  { value: "3", label: "3 months" },
  { value: "6", label: "6 months" },
  { value: "12", label: "1 year" },
  { value: "24", label: "2 years" },
] as const

type Lifetime = (typeof LIFETIMES)[number]["value"]

export function KeysTab({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const [lifetime, setLifetime] = useState<Lifetime | "">("")
  const unit = snapshot.setup.unit

  if (!unit || !snapshot.setup.attached) {
    return (
      <Card>
        <CardContent>
          <EmptyState
            title="No mint attached yet"
            body="Attach this processor to your mint in the Mint tab; its keys for the unit show up here."
          />
        </CardContent>
      </Card>
    )
  }
  if (snapshot.keysets_error) {
    return (
      <Alert>
        <TriangleAlert />
        <AlertTitle>Could not read the mint&apos;s keysets</AlertTitle>
        <AlertDescription>{snapshot.keysets_error}</AlertDescription>
      </Alert>
    )
  }

  const keysets = sortKeysets(snapshot.keysets, now)
  const active = keysets.find((keyset) => keyset.active)
  const check = snapshot.checklist.find((item) => item.id === "keyset")
  const planned = lifetime ? plannedExpiry(now, Number(lifetime)) : null

  return (
    <div className="grid gap-4">
      {check && (check.status === "warn" || check.status === "fail") && (
        <Alert variant="emphasis">
          <TriangleAlert />
          <AlertTitle>
            {!active
              ? `No active keys for ${unit.toUpperCase()}`
              : isKeysetExpired(active, now)
                ? "The active keys have expired"
                : `The active keys expire on ${formatDate(active.final_expiry!)}`}
          </AlertTitle>
          <AlertDescription>{check.remedy ?? check.detail}</AlertDescription>
        </Alert>
      )}

      <CurrentKeysCard unit={unit} keysets={keysets} active={active} now={now} />

      <TimelineCard keysets={keysets} now={now} planned={planned} />

      <RotateCard
        unit={unit}
        active={active}
        now={now}
        lifetime={lifetime}
        onLifetime={setLifetime}
        planned={planned}
      />

      <KeysetTableCard unit={unit} keysets={keysets} now={now} />
    </div>
  )
}

function CurrentKeysCard({
  unit,
  keysets,
  active,
  now,
}: {
  unit: string
  keysets: KeysetEntry[]
  active: KeysetEntry | undefined
  now: number
}) {
  const redeemOnly = keysets.filter((keyset) => keysetState(keyset, now) === "redeem")

  return (
    <Card>
      <CardHeader>
        <CardTitle>Keys for {unit.toUpperCase()}</CardTitle>
        <CardDescription>
          Your mint manages its keys. This console reads them and records when each one starts
          and stops issuing.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-2.5">
        {!active ? (
          <DetailRow label="Issuing keys">
            <span className="text-sm">None — the mint cannot issue new ecash for this unit.</span>
          </DetailRow>
        ) : (
          <>
            <DetailRow label="Issuing keys">
              <span className="inline-flex flex-wrap items-center gap-2">
                <MonoChip>{active.id}</MonoChip>
                <KeysetBadge keyset={active} now={now} />
              </span>
            </DetailRow>
            <DetailRow label="Issuing since">
              <span className="text-sm">
                {!active.first_seen_at
                  ? "Unknown"
                  : active.start_observed
                    ? formatDate(active.first_seen_at)
                    : `Before ${formatDate(active.first_seen_at)}, when this console first saw it`}
              </span>
            </DetailRow>
            <DetailRow label="New ecash expires">
              <span className="text-sm">
                {active.final_expiry == null
                  ? "Never — these keys have no expiry"
                  : `${formatDate(active.final_expiry)} · ${formatRelative(active.final_expiry, now)}`}
              </span>
            </DetailRow>
            <DetailRow label="Input fee">
              <span className="font-mono text-sm">{active.input_fee_ppk} ppk</span>
            </DetailRow>
          </>
        )}
        <DetailRow label="Older ecash">
          <span className="text-sm">{olderEcash(redeemOnly)}</span>
        </DetailRow>
      </CardContent>
    </Card>
  )
}

/** What happens to ecash on keys that no longer issue, in one sentence. */
function olderEcash(redeemOnly: KeysetEntry[]) {
  if (redeemOnly.length === 0) return "No redeem-only keys"
  const count = `${redeemOnly.length} redeem-only keyset${redeemOnly.length === 1 ? "" : "s"}`
  const withoutExpiry = redeemOnly.filter((keyset) => keyset.final_expiry == null).length
  if (withoutExpiry === redeemOnly.length) return `${count} without expiry — redeemable indefinitely`
  if (withoutExpiry > 0) return `${count}; ${withoutExpiry} without expiry — redeemable indefinitely`
  const last = Math.max(...redeemOnly.map((keyset) => keyset.final_expiry ?? 0))
  return `${count}, redeemable until ${formatDate(last)} at the latest`
}

function TimelineCard({
  keysets,
  now,
  planned,
}: {
  keysets: KeysetEntry[]
  now: number
  planned: number | null
}) {
  const [showExpired, setShowExpired] = useState(false)
  const expired = keysets.filter((keyset) => isKeysetExpired(keyset, now))
  const visible = showExpired ? keysets : keysets.filter((keyset) => !isKeysetExpired(keyset, now))

  return (
    <Card>
      <CardHeader>
        <CardTitle>Key timeline</CardTitle>
        <CardDescription>
          One bar per keyset: solid while it issues new ecash, hatched while its ecash can only
          be redeemed, ending at its final expiry.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-5">
        {keysets.length === 0 ? (
          <EmptyState
            title="No keys yet"
            body="The mint creates keys for the unit on its first start with the snippet applied."
          />
        ) : (
          <>
            <KeyTimeline keysets={visible} now={now} plannedExpiry={planned} />
            <div className="flex flex-wrap items-center justify-between gap-3">
              <KeyTimelineLegend showPlanned={planned != null} />
              {expired.length > 0 && (
                <Button variant="ghost" size="sm" onClick={() => setShowExpired(!showExpired)}>
                  {showExpired ? "Hide expired" : `Show ${expired.length} expired`}
                </Button>
              )}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}

/**
 * Issuing expiring ecash means rotating the keyset (the expiry is part of the
 * keyset id). The mint operator does that with cdk's own tooling — this card
 * only composes the exact command and says what it will do.
 */
function RotateCard({
  unit,
  active,
  now,
  lifetime,
  onLifetime,
  planned,
}: {
  unit: string
  active: KeysetEntry | undefined
  now: number
  lifetime: Lifetime | ""
  onLifetime: (lifetime: Lifetime) => void
  planned: number | null
}) {
  const fee = active?.input_fee_ppk ?? 0
  const command =
    planned != null
      ? `cdk-mint-cli rotate-next-keyset --unit ${unit} --input-fee-ppk ${fee} --final-expiry ${planned}`
      : null

  return (
    <Card>
      <CardHeader>
        <CardTitle>Issue ecash with an expiry</CardTitle>
        <CardDescription>
          A keyset&apos;s expiry is fixed when it is created, so expiring ecash needs new keys.
          Your mint creates them: pick a lifetime and run the command with cdk-mint-cli.
        </CardDescription>
        {command && (
          <CardAction>
            <CopyButton value={command} label="Copy rotation command" />
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-sm font-medium">New keys expire after</span>
          <SegmentedControl
            label="Lifetime of the new keys"
            value={lifetime as Lifetime}
            onChange={onLifetime}
            options={LIFETIMES}
          />
        </div>
        {command && planned != null ? (
          <>
            <pre className="m-0 overflow-x-auto rounded-md border bg-muted p-3 font-mono text-xs leading-5">
              {command}
            </pre>
            <ul className="m-0 grid list-disc gap-1.5 pl-5 text-sm text-muted-foreground">
              <li>
                New ecash expires on{" "}
                <span className="font-medium text-foreground">{formatDate(planned)}</span> (
                {formatRelative(planned, now)}); the timeline above previews the new keys.
              </li>
              <li>
                The current keys stop issuing at once. Their ecash stays redeemable{" "}
                {active?.final_expiry != null
                  ? `until ${formatDate(active.final_expiry)}`
                  : "indefinitely — they have no expiry"}
                .
              </li>
              <li>
                Ecash still on the new keys after that date can no longer be redeemed. Wallets
                move ecash onto the newest keys whenever they swap it, so rotate again well
                before then.
              </li>
              <li>
                Keep the fee ({fee} ppk) and the default amounts: if they differ from your
                mintd&apos;s configuration, the mint replaces these keys with ones that never
                expire on its next restart.
              </li>
              <li>
                Needs [mint_management_rpc] enabled on the mint and cdk-mint-cli, which is not
                part of the mintd image. It connects to https://127.0.0.1:8086 unless you pass
                --addr. This console picks up the new keys within five minutes.
              </li>
            </ul>
          </>
        ) : (
          <p className="m-0 text-sm text-muted-foreground">
            Pick a lifetime to see the command and what it changes.
          </p>
        )}
      </CardContent>
    </Card>
  )
}

function KeysetTableCard({
  unit,
  keysets,
  now,
}: {
  unit: string
  keysets: KeysetEntry[]
  now: number
}) {
  if (keysets.length === 0) return null
  return (
    <Card>
      <CardHeader>
        <CardTitle>All keysets for {unit.toUpperCase()}</CardTitle>
        <CardAction>
          <span className="text-sm text-muted-foreground">
            {keysets.length} keyset{keysets.length === 1 ? "" : "s"}
          </span>
        </CardAction>
      </CardHeader>
      <CardContent className="px-0">
        <Table className="min-w-[720px]">
          <TableHeader>
            <TableRow>
              <TableHead className="pl-6">Keyset</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Started</TableHead>
              <TableHead>Stopped issuing</TableHead>
              <TableHead>Final expiry</TableHead>
              <TableHead className="pr-6">Input fee</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {keysets.map((keyset) => (
              <TableRow key={keyset.id}>
                <TableCell className="pl-6">
                  <span title={keyset.id}>
                    <MonoChip className="max-w-[18ch] truncate">{keyset.id}</MonoChip>
                  </span>
                </TableCell>
                <TableCell>
                  <KeysetBadge keyset={keyset} now={now} />
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {!keyset.first_seen_at
                    ? "—"
                    : keyset.start_observed
                      ? formatDate(keyset.first_seen_at)
                      : `Before ${formatDate(keyset.first_seen_at)}`}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {keyset.active
                    ? "Still issuing"
                    : keyset.deactivated_at
                      ? formatDate(keyset.deactivated_at)
                      : keyset.first_seen_at
                        ? `Before ${formatDate(keyset.first_seen_at)}`
                        : "—"}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {keyset.final_expiry == null ? (
                    "None"
                  ) : (
                    <>
                      {formatDate(keyset.final_expiry)}
                      <span className="ml-1.5 text-xs">
                        {formatRelative(keyset.final_expiry, now)}
                      </span>
                    </>
                  )}
                </TableCell>
                <TableCell className="pr-6 font-mono">{keyset.input_fee_ppk}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}
