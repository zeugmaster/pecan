# Operations Guide

Procedures beyond the README's quick reference. Everything here assumes an
installation created by the installer (deployment files + `.env` + the
`mintctl` binary in one directory, default `~/pecan` for a normal user or
`/opt/pecan` for root on Linux).

## Installing and operating without root

Run the installer and `mintctl` as the same user, with a working `docker info`
and Docker Compose v2 supporting `up --wait --wait-timeout`. Normal users
get `~/pecan` and `~/.local/bin/mintctl`; root gets `/opt/pecan` and
`/usr/local/bin/mintctl`. `--dir` accepts an absolute or relative path. Add
`export PATH="$HOME/.local/bin:$PATH"` to your shell startup file if needed;
`~/pecan/mintctl` also works directly. A second install keeps an existing
`mintctl` link, and each new directory gets its own Compose project name.

Pecan uses your current Docker context. You can use either a daemon your
user has permission to access or [rootless Docker](https://docs.docker.com/engine/security/rootless/).
Installing system Docker with `--install-docker` requires root; setting up
rootless Docker is a separate prerequisite. If permissions were just
granted through group membership, log in again before retrying.

Rootless Docker may be unable to publish ports 80/443 under the host's
privileged-port settings. The installer warns about this before committing
configuration. Use `--behind-proxy --console-domain console.example.org`
with an existing proxy, or `--plain-http` with unprivileged ports on a
trusted LAN. See Docker's [privileged-port configuration](https://docs.docker.com/engine/security/rootless/tips/#exposing-privileged-ports)
if you want bundled Caddy with rootless Docker.

If an install fails after configuration was saved, run
`<install-dir>/mintctl start` to retry image downloads and startup. The
generated credentials remain in `.env` and `mint/mnemonic`; rerunning the
installer refuses to replace them. Downloads and configuration validation
before that point can be retried with the original install command.

For an existing root-owned installation, continue managing it as its owner;
new user-owned defaults do not move its files or volumes. Set
`MINTCTL_DIR=/path/to/install` to select an installation explicitly. Backup
archives and restored mint files are written by the invoking user, even
when Docker runs as root.

Scope note: this stack is the **branch processor** — the payment backend and
teller console that attaches to one cdk-mintd. If you run the mint yourself
(its seed, database, backups, keysets, TLS for the wallet-facing URL), it is
yours to run with cdk's own tooling; nothing here manages it. Installs
created with `--with-mint` additionally run the official `cashubtc/mintd`
image in the same compose project — see "Running the bundled mint" below for
what that changes.

## The .env file

`mintctl` and Docker Compose read all deployment configuration from
`<install-dir>/.env`. Every key is documented in `.env.example` next to it.
The important ones:

| Key | Meaning |
|---|---|
| `VERSION` | image tag to run (`vX.Y.Z`); changed by `mintctl update` |
| `COMPOSE_PROJECT_NAME` | prefixes container and volume names; never change it on an existing install (the volumes would be orphaned) |
| `UI_PORT` | host port for the operator console |
| `BIND_ADDR` | host interface for the console port: `127.0.0.1` in TLS and behind-proxy modes, `0.0.0.0` in plain-HTTP mode |
| `COMPOSE_PROFILES` | stack shape: `tls` enables the bundled Caddy, `mint` the bundled mint, `tls,mint` both |
| `CONSOLE_DOMAIN` | hostname Caddy serves and gets certificates for |
| `INITIAL_ADMIN_PASSWORD` | first boot only — seeds the admin account (change forced at first sign-in); inert once `users.json` exists |
| `ACME_EMAIL` | optional; `mintctl update` re-applies it to the Caddyfile |
| `GRPC_BIND_ADDR` / `GRPC_PORT` | where the payment gRPC is published for your mintd (loopback:50051 by default; plaintext gRPC has no auth — firewall it or use TLS). `GRPC_PORT` also feeds the console's attachment prefill |
| `GRPC_TLS_DIR` | optional mutual TLS for the payment gRPC (see `.env.example`) |
| `INITIAL_UNIT` / `INITIAL_MINT_URL` / `INITIAL_ADVERTISED_GRPC` | first boot only — pre-seed the Mint-tab attachment (bundled installs and headless attach flags); inert once `setup.json` exists |
| `MINT_VERSION` | bundled mint's image tag; moves independently of `VERSION` (`mintctl update --mint-version`) |
| `MINT_PORT` / `MINT_BIND_ADDR` | host port and interface for the bundled mint's wallet-facing API |
| `MINT_DOMAIN` | the bundled mint's public hostname (Caddy site + compose network alias) |

Prefer `mintctl domain` over hand-editing the access keys; for anything else,
apply `.env` edits with `mintctl stop && mintctl start`.

## Attaching your mint

(Only for processor-only installs — a bundled mint arrives already attached.)
Everything happens in the console's **Mint tab**:

1. Set the **unit** (e.g. `ora`) and the mint's public **URL** — the same URL
   wallets use. Setup applies live; no restart. (Headless installs can
   pre-seed this step with `mintctl install --unit <u> --mint-url <url>`.)
2. Apply the generated **config snippet** to your mintd. cdk-mintd 0.18+
   keeps its configuration in the mint database, so a mint.toml on disk is an
   import document: fresh mint — `cdk-mintd config init --file mint.toml`,
   then start it; running mint — `cdk-mintd config export --file mint.toml`,
   merge the snippet, `cdk-mintd config apply --file mint.toml`, restart.
   Editing the file without `config apply` changes nothing. A complete
   example lives at `docs/examples/mint.toml`.
3. Watch the **attachment checklist** settle: reachable → advertised →
   linked → keys → end-to-end. Every failing check comes with the exact
   remedy.
4. The **self-test** (runs automatically on first attach; button to re-run)
   creates one deposit and one payout quote at the mint, verifies both arrive
   at this processor, then voids them. It also measures the mint's quote
   lifetimes and warns when they are too short for a counter visit.

Compatibility: your mintd must run cdk-mintd v0.18.0-rc.0 or later — the
first release whose payment-processor protocol (4.0.0, strict equality at
connect time) carries the quote id on custom mint quotes. The official
docker image `cashubtc/mintd:0.18.0-rc.0` is the easiest way to run one.

The **unit locks** after the first successful self-test: issued ecash and
quotes reference it. Changing a locked unit means starting over deliberately —
stop the processor, edit `unit` and `unit_locked` in `setup.json` on the
config volume, and accept that existing tickets for the old unit remain
history-only.

## Running the bundled mint

Installs created with `--with-mint` (or the wizard's "processor and a new
mint" choice) run the official `cashubtc/mintd` image in the same compose
project, pre-attached and self-tested by the time the installer finishes.
What changes compared to a processor-only install:

- **The seed is yours to keep.** The installer generates the mint's 12-word
  BIP39 seed, shows it exactly once in the finish screen, and stores it at
  `<install-dir>/mint/mnemonic` (0600). It is the mint: anyone holding it can
  issue your ecash; without it a lost server means lost keys. Write it down,
  store it offline.
- **The mint's config is database-authoritative.** The installer's
  `<install-dir>/mint/config.toml` was imported once by `cdk-mintd config
  init` on first start. To change mint settings later (metadata, quote TTLs,
  limits), edit the file and apply it explicitly:

  ```sh
  cd <install-dir>
  docker compose -f docker-compose.yml --project-directory . \
    run --rm --no-deps mintd cdk-mintd --work-dir /data config apply --file /config/mint.toml
  ./mintctl start
  ```

- **`mintctl backup` includes the mint** — the `mintd-data` volume (mint
  database) and the `mint/` directory (config + seed). Such an archive can
  issue your ecash: encrypt it. `mintctl restore` restores mint and
  processor state together and refuses archives whose shape (with/without
  mint) does not match the install.
- **Updates never touch the mint implicitly.** `mintctl update` moves the
  processor stack only; the mint's image is pinned by `MINT_VERSION` and
  upgraded deliberately with `mintctl update --mint-version <tag>`.
- **`mintctl status`** additionally probes the mint's `/v1/info`; `mintctl
  logs mintd` follows its log.
- For immediate-effect capability changes, cdk's management RPC
  (`cdk-mint-cli`) remains available if you enable `[mint_management_rpc]`
  in the mint's config yourself.

## Persistence

Docker Compose uses named volumes:

| Volume | Data |
|---|---|
| `config-data` | processor config (`setup.json`), user accounts (`users.json`) |
| `processor-data` | branch ticket store (`tickets.json`), login sessions |
| `caddy-data`, `caddy-config` | TLS certificates and Caddy state (TLS mode only) |
| `mintd-data` | the bundled mint's database and work dir (bundled installs only) |

Updates and recreates keep these volumes. `mintctl backup` / `restore` are
the supported way to snapshot and move them. An externally-run mint's data
lives wherever you run that mint — it is not part of this stack. To reset
the processor for a fresh local demo: `docker compose down -v`.

## Backup and restore drill

```sh
mintctl backup /root/branch-$(date +%F).tar.gz     # brief downtime
```

The archive contains the processor's two data volumes and `.env`: operator
accounts (password hashes), the attachment configuration, and the ticket
ledger. On bundled-mint installs it additionally contains the mint's
database volume and the `mint/` directory — **including the seed**, so the
archive can issue your ecash. Encrypt it (e.g. `age`/`gpg`) and store it off
the server. For a processor-only install, no mint data is included — the
mint's seed and database are backed up by whoever operates the mint.

Restore onto the same install:

```sh
mintctl restore /root/branch-2026-08-04.tar.gz     # replaces the processor's state
mintctl status
```

Verify after any restore: console reachable, sign-in works, the Mint tab
checklist settles green against your mint, the teller's recent activity shows
the expected history.

## Migrating to a new server

1. Old server: `mintctl backup`, copy the archive off.
2. New server: run the installer (same console domain if you use one; DNS can
   move afterwards — certificates re-issue automatically).
3. New server: `mintctl restore <archive>`.
4. Update your mintd's `[grpc_processor].address` if the processor's address
   changed, restart the mintd, and watch the checklist settle.
5. Point DNS at the new server; verify; decommission the old install
   (`mintctl uninstall --purge` only after the new one is verified).

## Migrating from the bundled mint (pre-0.2 installs)

Installs created before 0.2 bundled a managed cdk-mintd in the same compose
project. `mintctl update` refuses to update them in place, because the new
compose file has no mint service and the update would remove the mint
container. To migrate:

1. Take a `mintctl backup` with the OLD version (it still includes the
   `mint-data` volume).
2. Move the mint out: run cdk-mintd yourself — reuse the old `mint-data`
   volume (or copy it) and the last generated `mint.toml` from the config
   volume. The mint's recovery mnemonic is inside that `mint.toml`; it is
   yours now — back it up.
3. After verifying that the mint runs independently, remove the obsolete
   `MINT_MODE=...` line from the old install's `.env`, then update the install.
   Keep the old backup. On first start the processor migrates its
   configuration automatically: the old `setup.json` (which also contains the
   mnemonic) is preserved as `setup.json.v3-managed.bak` on the config volume
   and is never deleted; the console shows a one-time notice.
4. In the Mint tab, confirm the attachment (URL of your now-self-run mint)
   and run the self-test. Note: the mint must run cdk-mintd v0.18.0-rc.0 or
   later (the old bundled build predates the required protocol), and its old
   generated `mint.toml` needs `cdk-mintd config migrate` first — 0.18
   refuses `--config` at startup and literal secrets in the TOML.
5. The old `mint-data` volume is no longer referenced by compose; remove it
   only after the mint runs elsewhere and is verified.

## Running a second instance on one host

```sh
curl -fsSL .../install.sh | bash -s -- --dir /opt/second-branch \
    --ui-port 19090 --grpc-port 60051 --console-domain console2.example.org
```

Each install directory is fully self-contained (own project name, volumes,
ports — including the published gRPC port). TLS mode note: ports 80/443 can
only be bound by one instance — for several TLS instances, run one shared
reverse proxy instead (next section).

## Bringing your own reverse proxy

If the host already runs nginx/Traefik/Caddy, the installer detects the
occupied ports 80/443 and suggests behind-proxy mode; `mintctl domain`
switches an existing install the same way. In that mode the console binds
loopback only and a ready-to-paste server block lands in
`<install-dir>/proxy-snippets/` (Caddy and nginx flavors, with the details
that matter baked in):

- targets `127.0.0.1:$UI_PORT`,
- forwards `X-Forwarded-Proto` — the processor uses it to mark session
  cookies `Secure`,
- streams the console's SSE endpoint (`GET /events`) unbuffered.

The payment gRPC is **not** proxied — your mintd connects to it directly.

## Changing how the console is reached

`mintctl domain` re-runs the access step on a running install — plain HTTP →
domain with automatic HTTPS, a new domain, or behind-your-own-proxy — with
the same DNS live-check and certificate wait as the installer
(non-interactive: `mintctl domain --console-domain new.example.org --yes`).
It updates `.env`, re-renders the proxy snippet when applicable, and
recreates the affected containers. On bundled-mint installs it also carries
the mint's hostname through (`--mint-domain`); remember that changing the
mint's hostname changes the URL wallets — and the console's Mint tab —
point at.

## Updates and recovery

```sh
mintctl update --check                  # preview, without changing the install
mintctl update                         # latest console/processor and CLI
mintctl update --with-mint              # also the target release's tested mint
mintctl update --with-mint --check      # preview both version changes
mintctl update --version vX.Y.Z         # choose a particular Pecan release
mintctl update --mint-version <tag>     # mint only; keep the current console
mintctl update --version vX.Y.Z --mint-version <tag>  # explicit pairing
mintctl update --with-mint --yes        # unattended update of both
```

The default update preserves `MINT_VERSION`. `--with-mint` reads the tested
mint pin from the **target release**, rather than guessing the newest
upstream mint. An explicit `--mint-version` alone updates only the mint;
combine it with `--version` to move both. External mints are managed with
their own tooling. Pre-0.2 managed mints need the migration above.

If your installed CLI predates these options, use the current bootstrap to
perform the update (run as the installation's owner, adjusting the path):

```sh
curl -fsSL https://raw.githubusercontent.com/zeugmaster/pecan/main/install.sh \
  | MINTCTL_DIR="$HOME/pecan" bash -s -- update --with-mint
```

The CLI displays the proposed versions and asks for confirmation; `--yes`
approves unattended operation. It stages the deployment files and verified
CLI binary, validates Compose configuration, and pulls the selected images
before changing live files. Failed downloads, invalid configuration, or a
failed backup leave the existing version pins in place.

Before activation it briefly stops the running services for a consistent
snapshot, then restarts them. The private directory
`<install-dir>/updates/before-*/` contains `backup.tar.gz`, previous
deployment files and the CLI, plus `RECOVERY.txt`. The archive includes a
bundled mint's database and seed. Encrypt off-server copies and remove old
local backups when your retention policy allows. Backups need free disk
space for the full data snapshot and the `debian:bookworm-slim` helper image
(pulled before downtime if absent).

Only selected services are recreated. A console-only update does not
change the mint pin or recreate its container, although it pauses the mint
for the snapshot. Compose waits for container health before reporting
success; verify the Mint tab's attachment checklist after changing the
mint/processor pairing. Existing named volumes, unselected containers and
old images are retained. Deployment templates are replaced on console
updates; the snapshot retains any local edits for review and reapplication.

If activation fails, the target pins and recovery snapshot remain. Inspect
`mintctl logs` and retry `mintctl start` after fixing the cause. **Do not
downgrade a mint against a database that a newer version may have migrated.**
For deliberate recovery, stop the stack, restore the saved deployment files
and CLI (preserving permissions), then use that CLI's `restore` command with
the saved archive, as described in `RECOVERY.txt`. Restoring discards activity
since the snapshot. Pecan does not attempt an automatic database downgrade.

## Health and monitoring

- `GET http://127.0.0.1:$UI_PORT/healthz` (or `https://<console-domain>/healthz`)
  — unauthenticated liveness + running version; wire it into uptime checks.
  An unattached processor is healthy by design — attachment truth lives in
  the authenticated checklist.
- `docker ps` shows the container healthcheck; `mintctl status` summarizes.
- Subsystem detail (mint reachable, advertisement, gRPC link, keys,
  end-to-end) is the Mint tab's checklist; the Overview tiles mirror it.
