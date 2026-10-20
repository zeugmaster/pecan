import type { ReactNode } from "react"

import type { Ticket } from "@/lib/api"
import { formatAmount } from "@/lib/format"
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
import { StatusBadge } from "@/components/shared/badges"
import { EmptyState, MonoChip, Timestamp } from "@/components/shared/bits"

/** Settled and voided tickets, one row each; the caller supplies the card. */
export function TicketTable({
  tickets,
  now,
  showNotes,
  showOperator,
}: {
  tickets: Ticket[]
  now: number
  showNotes?: boolean
  showOperator?: boolean
}) {
  return (
    <Table className="min-w-[560px] [&_td:last-child]:pr-6 [&_th:last-child]:pr-6">
      <TableHeader>
        <TableRow>
          <TableHead className="pl-6">Ticket</TableHead>
          <TableHead>Kind</TableHead>
          <TableHead>Amount</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>When</TableHead>
          {showOperator && <TableHead>By</TableHead>}
          {showNotes && <TableHead>Notes</TableHead>}
        </TableRow>
      </TableHeader>
      <TableBody>
        {tickets.map((ticket) => (
          <TableRow key={ticket.id}>
            <TableCell className="pl-6">
              <MonoChip>{ticket.short_id}</MonoChip>
            </TableCell>
            <TableCell className="text-muted-foreground">
              {ticket.kind === "incoming" ? "Deposit" : "Payout"}
            </TableCell>
            <TableCell className="font-medium">
              {formatAmount(ticket.amount, ticket.unit)}
            </TableCell>
            <TableCell>
              <StatusBadge status={ticket.status} />
            </TableCell>
            <TableCell className="text-muted-foreground">
              <Timestamp ts={ticket.closed_at} now={now} />
            </TableCell>
            {showOperator && (
              <TableCell className="font-mono text-muted-foreground">
                {ticket.operator ?? "-"}
              </TableCell>
            )}
            {showNotes && (
              <TableCell className="max-w-[24ch] truncate text-muted-foreground">
                {ticket.notes ?? "-"}
              </TableCell>
            )}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

export function ActivityTable({
  title,
  description,
  action,
  tickets,
  now,
  showNotes,
  showOperator,
}: {
  title: string
  description?: ReactNode
  /** Header action; defaults to the row count. */
  action?: ReactNode
  tickets: Ticket[]
  now: number
  showNotes?: boolean
  showOperator?: boolean
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        <CardAction>
          {action ?? <span className="text-sm text-muted-foreground">{tickets.length} rows</span>}
        </CardAction>
      </CardHeader>
      <CardContent className="px-0">
        {tickets.length === 0 ? (
          <EmptyState
            title="No settled operations yet"
            body="Completed deposits and payouts will appear here."
          />
        ) : (
          <TicketTable
            tickets={tickets}
            now={now}
            showNotes={showNotes}
            showOperator={showOperator}
          />
        )}
      </CardContent>
    </Card>
  )
}
