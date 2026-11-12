import { useState, type FormEvent } from "react"
import { KeyRound, Trash2, TriangleAlert } from "lucide-react"
import { toast } from "sonner"

import {
  changePassword,
  createUser,
  deleteUser,
  resetUserPassword,
  type AppSnapshot,
} from "@/lib/api"
import { useSnapshot } from "@/lib/snapshot"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog"
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Field } from "@/components/shared/bits"
import { WalletAccess } from "@/components/console/wallet-access"
import { WalletCustomers } from "@/components/console/wallet-customers"

export function AccessTab({ snapshot }: { snapshot: AppSnapshot }) {
  const me = snapshot.session.username

  return (
    <div className="grid gap-4">
      <WalletAccess key={snapshot.setup.mint_url} mintUrl={snapshot.setup.mint_url} />
      <WalletCustomers key={`customers-${snapshot.setup.mint_url}`} />
      {snapshot.demo_password_active && (
        <Alert variant="emphasis">
          <TriangleAlert />
          <AlertTitle>Demo credentials are active</AlertTitle>
          <AlertDescription>
            The admin account still uses the demo password. Change it below before real use.
          </AlertDescription>
        </Alert>
      )}

      <Card>
        <CardHeader>
          <CardTitle>Pecan staff</CardTitle>
          <CardDescription>
            Every user signs in to both the console and the teller.
          </CardDescription>
          <CardAction>
            <AddUserDialog minLength={snapshot.password_min_length} />
          </CardAction>
        </CardHeader>
        <CardContent className="px-0">
          <Table className="min-w-[480px]">
            <TableHeader>
              <TableRow>
                <TableHead className="pl-6">Username</TableHead>
                <TableHead>Created</TableHead>
                <TableHead className="w-24 pr-6 text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {snapshot.users.map((user) => {
                const self = user.username === me
                return (
                  <TableRow key={user.username}>
                    <TableCell className="pl-6">
                      <span className="inline-flex items-center gap-2 font-mono">
                        {user.username}
                        {self && <Badge variant="solid">You</Badge>}
                      </span>
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {new Date(user.created_at * 1000).toLocaleDateString()}
                    </TableCell>
                    <TableCell className="pr-6">
                      <div className="flex justify-end gap-1">
                        <ResetPasswordDialog
                          username={user.username}
                          disabled={self}
                          minLength={snapshot.password_min_length}
                        />
                        <DeleteUserDialog username={user.username} disabled={self} />
                      </div>
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <ChangePasswordCard minLength={snapshot.password_min_length} />
    </div>
  )
}

function AddUserDialog({ minLength }: { minLength: number }) {
  const { refresh } = useSnapshot()
  const [open, setOpen] = useState(false)
  const [username, setUsername] = useState("")
  const [password, setPassword] = useState("")
  const [confirm, setConfirm] = useState("")
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (password !== confirm) {
      toast.error("Passwords do not match.")
      return
    }
    setBusy(true)
    try {
      await createUser(username, password, confirm)
      toast.success(`Added user ${username.toLowerCase()}`)
      setOpen(false)
      setUsername("")
      setPassword("")
      setConfirm("")
      await refresh()
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not add the user.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm">Add user</Button>
      </DialogTrigger>
      <DialogContent>
        <form className="grid gap-4" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>Add a user</DialogTitle>
            <DialogDescription>
              Lowercase username; the password needs at least {minLength} characters.
            </DialogDescription>
          </DialogHeader>
          <Field label="Username" htmlFor="add-user-name">
            <Input
              id="add-user-name"
              className="font-mono"
              value={username}
              onChange={(event) => setUsername(event.target.value.toLowerCase())}
              autoComplete="off"
              required
              autoFocus
            />
          </Field>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Password" htmlFor="add-user-password">
              <Input
                id="add-user-password"
                type="password"
                minLength={minLength}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="new-password"
                required
              />
            </Field>
            <Field label="Confirm password" htmlFor="add-user-confirm">
              <Input
                id="add-user-confirm"
                type="password"
                minLength={minLength}
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
                autoComplete="new-password"
                required
              />
            </Field>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" loading={busy}>
              Add user
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ResetPasswordDialog({
  username,
  disabled,
  minLength,
}: {
  username: string
  disabled: boolean
  minLength: number
}) {
  const { refresh } = useSnapshot()
  const [open, setOpen] = useState(false)
  const [password, setPassword] = useState("")
  const [confirm, setConfirm] = useState("")
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    try {
      await resetUserPassword(username, password, confirm)
      toast.success(`Password reset for ${username}. Their sessions were signed out.`)
      setOpen(false)
      setPassword("")
      setConfirm("")
      await refresh()
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not reset the password.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <Tooltip>
        <TooltipTrigger asChild>
          <span>
            <DialogTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Reset password for ${username}`}
                disabled={disabled}
              >
                <KeyRound />
              </Button>
            </DialogTrigger>
          </span>
        </TooltipTrigger>
        <TooltipContent>{disabled ? "Use “Change your password” below" : "Reset password"}</TooltipContent>
      </Tooltip>
      <DialogContent>
        <form className="grid gap-4" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>Reset password for {username}</DialogTitle>
            <DialogDescription>
              Their existing sessions are signed out immediately.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="New password" htmlFor="reset-password">
              <Input
                id="reset-password"
                type="password"
                minLength={minLength}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="new-password"
                required
                autoFocus
              />
            </Field>
            <Field label="Confirm password" htmlFor="reset-confirm">
              <Input
                id="reset-confirm"
                type="password"
                minLength={minLength}
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
                autoComplete="new-password"
                required
              />
            </Field>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" loading={busy}>
              Reset password
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function DeleteUserDialog({ username, disabled }: { username: string; disabled: boolean }) {
  const { refresh } = useSnapshot()

  async function remove() {
    try {
      await deleteUser(username)
      toast.success(`Deleted user ${username}`)
      await refresh()
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not delete the user.")
    }
  }

  return (
    <AlertDialog>
      <Tooltip>
        <TooltipTrigger asChild>
          <span>
            <AlertDialogTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Delete ${username}`}
                disabled={disabled}
              >
                <Trash2 />
              </Button>
            </AlertDialogTrigger>
          </span>
        </TooltipTrigger>
        <TooltipContent>{disabled ? "You cannot delete yourself" : "Delete user"}</TooltipContent>
      </Tooltip>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {username}?</AlertDialogTitle>
          <AlertDialogDescription>
            Their sessions are signed out immediately. This cannot be undone.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep user</AlertDialogCancel>
          <AlertDialogAction onClick={() => void remove()}>Delete user</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

function ChangePasswordCard({ minLength }: { minLength: number }) {
  const { refresh } = useSnapshot()
  const [current, setCurrent] = useState("")
  const [password, setPassword] = useState("")
  const [confirm, setConfirm] = useState("")
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    try {
      await changePassword(current, password, confirm)
      toast.success("Password changed")
      setCurrent("")
      setPassword("")
      setConfirm("")
      await refresh()
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "Could not change the password.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Change your password</CardTitle>
        <CardDescription>
          At least {minLength} characters. Your other sessions are signed out.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form className="grid gap-4" onSubmit={submit}>
          <div className="grid gap-3 sm:grid-cols-3">
            <Field label="Current password" htmlFor="pw-current">
              <Input
                id="pw-current"
                type="password"
                value={current}
                onChange={(event) => setCurrent(event.target.value)}
                autoComplete="current-password"
                required
              />
            </Field>
            <Field label="New password" htmlFor="pw-new">
              <Input
                id="pw-new"
                type="password"
                minLength={minLength}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                autoComplete="new-password"
                required
              />
            </Field>
            <Field label="Confirm new password" htmlFor="pw-confirm">
              <Input
                id="pw-confirm"
                type="password"
                minLength={minLength}
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
                autoComplete="new-password"
                required
              />
            </Field>
          </div>
          <div>
            <Button type="submit" loading={busy}>
              Change password
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  )
}
