import { useState } from "react"
import {
  CalendarClock,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleDot,
  PlusCircle,
  TriangleAlert,
  Wallet,
} from "lucide-react"

import type { AppSnapshot, CheckStatus, ChecklistItem } from "@/lib/api"
import { formatDate, formatRelative, formatSignedAmount } from "@/lib/format"
import { isKeysetExpired } from "@/lib/keysets"
import { settledKey } from "@/lib/use-circulation"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { buttonVariants } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { ActivityTable } from "@/components/shared/activity-table"
import { SegmentedControl } from "@/components/shared/bits"
import {
  CHART_RANGES,
  CirculationChart,
  type ChartRange,
} from "@/components/shared/circulation-chart"
import { StatTile } from "@/components/shared/stat-tile"

function statusIcon(status: CheckStatus | undefined) {
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

/** Value + detail for the "Ecash expiry" tile, from the issuing keyset. */
function ecashExpiry(snapshot: AppSnapshot, now: number): { value: string; detail: string } {
  if (!snapshot.setup.attached) return { value: "—", detail: "Attach a mint to see its keys" }
  if (snapshot.keysets_error) return { value: "—", detail: "The mint's keys could not be read" }
  const active = snapshot.keysets.find((keyset) => keyset.active)
  if (!active) return { value: "No keys", detail: "The mint cannot issue ecash for this unit" }
  if (active.final_expiry == null) return { value: "None", detail: "Ecash issued now never expires" }
  if (isKeysetExpired(active, now))
    return { value: "Expired", detail: `The issuing keys expired ${formatDate(active.final_expiry)}` }
  return {
    value: formatDate(active.final_expiry),
    detail: `Ecash issued now expires ${formatRelative(active.final_expiry, now)}`,
  }
}

export function OverviewTab({ snapshot, now }: { snapshot: AppSnapshot; now: number }) {
  const [range, setRange] = useState<ChartRange>("30")
  const setup = snapshot.setup
  const expiry = ecashExpiry(snapshot, now)
  const byId = (id: string): ChecklistItem | undefined =>
    snapshot.checklist.find((check) => check.id === id)
  const reachable = byId("reachable")
  const linked = byId("linked")
  const worst: CheckStatus =
    (["fail", "warn", "unknown"] as CheckStatus[]).find((status) =>
      snapshot.checklist.some((check) => check.status === status),
    ) ?? "ok"

  return (
    <div className="grid gap-4">
      {!setup.setup_complete && (
        <Alert variant="emphasis">
          <PlusCircle />
          <AlertTitle>Not attached to a mint yet</AlertTitle>
          <AlertDescription>
            This processor serves one unit for a cdk mint you operate. Set the unit and mint URL
            in the{" "}
            <a className="underline underline-offset-2" href="#mint">
              Mint tab
            </a>
            , apply the config snippet to your mintd, and the checklist settles itself.
          </AlertDescription>
        </Alert>
      )}

      <section className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <StatTile
          label="Mint"
          value={
            reachable?.status === "ok"
              ? "Online"
              : reachable?.status === "fail"
                ? "Offline"
                : "Not attached"
          }
          detail={
            snapshot.mint_identity?.name ??
            (setup.mint_url || "Set the mint URL in the Mint tab")
          }
          icon={statusIcon(reachable?.status)}
        />
        <StatTile
          label="Payment link"
          value={
            linked?.status === "ok"
              ? "Linked"
              : linked?.status === "warn"
                ? "Incomplete"
                : linked?.status === "fail"
                  ? "Waiting"
                  : "—"
          }
          detail={
            linked?.status === "ok"
              ? "The mint is attached to this processor"
              : "The mint has not attached to this processor yet"
          }
          icon={statusIcon(linked?.status)}
        />
        <StatTile
          label="Setup"
          value={
            worst === "ok"
              ? "Ready"
              : worst === "fail"
                ? "Needs attention"
                : worst === "warn"
                  ? "Check warnings"
                  : "Incomplete"
          }
          detail="Details in the Mint tab checklist"
          icon={statusIcon(worst)}
        />
        <StatTile
          label="Open quotes"
          value={String(snapshot.open_quotes.length)}
          detail="Wallet-created quotes awaiting the counter"
          icon={<CircleDot />}
        />
        <StatTile
          label="Net settled"
          value={setup.unit ? formatSignedAmount(snapshot.summary.net_issued, setup.unit) : "—"}
          detail={
            setup.unit
              ? "Settled deposits minus payouts at this counter"
              : "Set a unit to start"
          }
          icon={<Wallet />}
        />
        <StatTile
          label="Ecash expiry"
          value={expiry.value}
          detail={expiry.detail}
          icon={<CalendarClock />}
        />
      </section>

      <Card>
        <CardHeader>
          <CardTitle>Settled activity</CardTitle>
          <CardDescription>
            Teller ledger over time: net of settled deposits and payouts, per day.
          </CardDescription>
          <CardAction>
            <SegmentedControl
              label="Chart range"
              value={range}
              onChange={setRange}
              options={CHART_RANGES}
            />
          </CardAction>
        </CardHeader>
        <CardContent>
          <CirculationChart unit={setup.unit} range={range} refreshKey={settledKey(snapshot)} />
        </CardContent>
      </Card>

      <ActivityTable
        title="Recent settled activity"
        tickets={snapshot.recent_done.slice(0, 8)}
        now={now}
        action={
          <a href="#activity" className={buttonVariants({ variant: "outline", size: "sm" })}>
            View all
          </a>
        }
      />
    </div>
  )
}
