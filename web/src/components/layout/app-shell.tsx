import type { ReactNode } from "react"
import { LogOut, RefreshCw } from "lucide-react"

import { logout } from "@/lib/api"
import { navigate, usePathname } from "@/lib/router"
import { useSnapshot } from "@/lib/snapshot"
import { cn } from "@/lib/utils"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"

const NAV = [
  { path: "/", label: "Console" },
  { path: "/teller", label: "Teller" },
]

export function AppShell({ children }: { children: ReactNode }) {
  const { snapshot, refresh } = useSnapshot()
  const pathname = usePathname()
  const openQuotes = snapshot.open_quotes.length

  async function signOut() {
    await logout()
    navigate("/login")
  }

  return (
    <div className="min-h-screen">
      <header className="sticky top-0 z-40 border-b bg-background">
        <div className="mx-auto flex h-14 w-full max-w-6xl items-center gap-4 px-4">
          <a
            href="/"
            onClick={(event) => {
              event.preventDefault()
              navigate("/")
            }}
            className="flex min-w-0 items-center gap-2.5"
          >
            <span className="grid size-7 shrink-0 place-items-center rounded-md bg-primary text-sm font-bold text-primary-foreground">
              ◐
            </span>
            <span className="truncate text-sm font-semibold">
              {snapshot.mint_identity?.name || "Pecan console"}
            </span>
          </a>

          <nav className="flex items-center gap-1">
            {NAV.map((item) => {
              const active =
                item.path === "/" ? pathname !== "/teller" : pathname.startsWith(item.path)
              return (
                <a
                  key={item.path}
                  href={item.path}
                  onClick={(event) => {
                    event.preventDefault()
                    navigate(item.path)
                  }}
                  className={cn(
                    "inline-flex h-8 items-center rounded-md px-3 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                    active && "bg-accent text-foreground",
                  )}
                >
                  {item.label}
                </a>
              )
            })}
          </nav>

          <div className="ml-auto flex items-center gap-1.5">
            {openQuotes > 0 && (
              <a
                href="/teller"
                onClick={(event) => {
                  event.preventDefault()
                  navigate("/teller")
                }}
              >
                <Badge variant="outline">
                  {openQuotes} open
                  <span className="max-sm:hidden">quote{openQuotes === 1 ? "" : "s"}</span>
                </Badge>
              </a>
            )}
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Refresh"
                  onClick={() => void refresh()}
                >
                  <RefreshCw />
                </Button>
              </TooltipTrigger>
              <TooltipContent>Refresh</TooltipContent>
            </Tooltip>
            <Button variant="ghost" size="sm" onClick={() => void signOut()}>
              <LogOut />
              <span className="max-sm:sr-only">Sign out</span>
            </Button>
          </div>
        </div>
      </header>

      <main className="mx-auto grid w-full max-w-6xl gap-6 px-4 py-6 [&>*]:min-w-0">{children}</main>
    </div>
  )
}
