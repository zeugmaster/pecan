import { useHashTab } from "@/lib/router"
import { useSnapshot } from "@/lib/snapshot"
import { useNow } from "@/lib/use-now"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { PageHeader } from "@/components/layout/page-header"
import { AccessTab } from "@/components/console/access-tab"
import { ActivityTab } from "@/components/console/activity-tab"
import { KeysTab } from "@/components/console/keys-tab"
import { MintTab } from "@/components/console/mint-tab"
import { OverviewTab } from "@/components/console/overview-tab"

const TAB_NAMES = ["overview", "mint", "keys", "activity", "access"]

function AttentionDot({ label }: { label: string }) {
  return (
    <span
      className="absolute right-0.5 top-1 size-1.5 rounded-full bg-foreground sm:right-1.5 sm:top-1.5"
      aria-label={label}
    />
  )
}

export function ConsolePage() {
  const { snapshot } = useSnapshot()
  // Ages and expiry countdowns keep moving between snapshot refreshes.
  const now = useNow(snapshot.now, 15_000)
  const [rawTab, setTab] = useHashTab("overview")
  const tab = TAB_NAMES.includes(rawTab) ? rawTab : "overview"

  const needsAttention = (check: { status: string }) =>
    check.status === "fail" || check.status === "warn"
  const mintNeedsAttention = snapshot.checklist.some(needsAttention)
  const keysNeedAttention = snapshot.checklist.some(
    (check) => check.id === "keyset" && needsAttention(check),
  )

  return (
    <>
      <PageHeader
        title={snapshot.mint_identity?.name || "Operator console"}
        description={
          snapshot.setup.mint_url ||
          "No mint attached yet — start in the Mint tab."
        }
      />
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList className="w-full max-w-xl">
          <TabsTrigger value="overview" className="px-2 sm:px-3">Overview</TabsTrigger>
          <TabsTrigger value="mint" className="relative px-2 sm:px-3">
            Mint
            {mintNeedsAttention && <AttentionDot label="Mint attachment needs attention" />}
          </TabsTrigger>
          <TabsTrigger value="keys" className="relative px-2 sm:px-3">
            Keys
            {keysNeedAttention && <AttentionDot label="Mint keys need attention" />}
          </TabsTrigger>
          <TabsTrigger value="activity" className="px-2 sm:px-3">Activity</TabsTrigger>
          <TabsTrigger value="access" className="relative px-2 sm:px-3">
            Access
            {snapshot.demo_password_active && <AttentionDot label="Demo password active" />}
          </TabsTrigger>
        </TabsList>
        <TabsContent value="overview">
          <OverviewTab snapshot={snapshot} now={now} />
        </TabsContent>
        <TabsContent value="mint">
          <MintTab snapshot={snapshot} now={now} />
        </TabsContent>
        <TabsContent value="keys">
          <KeysTab snapshot={snapshot} now={now} />
        </TabsContent>
        <TabsContent value="activity">
          <ActivityTab snapshot={snapshot} now={now} />
        </TabsContent>
        <TabsContent value="access">
          <AccessTab snapshot={snapshot} />
        </TabsContent>
      </Tabs>
    </>
  )
}
