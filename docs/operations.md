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
   import document: fresh mint — `cdk-mintd config init --new-mint --file mint.toml`,
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

Compatibility: wallet authentication is tested against cdk-mintd **0.18.2**,
with payment-processor protocol **4.0.0**. Pecan's dependencies, installer and
bundled image default are pinned to this release. Existing installations keep
their explicit image pin until upgraded; changing a default does not upgrade a
running mint. Back up its database and seed before updating it.

The **unit locks** after the first successful self-test: issued ecash and
quotes reference it. Changing a locked unit means starting over deliberately —
stop the processor, edit `unit` and `unit_locked` in `setup.json` on the
config volume, and accept that existing tickets for the old unit remain
history-only.

## Wallet authentication

Open **Access → Wallet access**. Pecan reads NUT-21 and NUT-22 from the attached
mint's `/v1/info` and shows the active policy for `branch`. The endpoint disclosure
also shows policies for other methods. If discovery fails, the active policy is
unknown; a saved draft is never treated as evidence of activation.

Choose **Open**, **Clear auth**, or **Blind auth** for deposit quotes, quote
lookups, issuance, withdrawal quotes, redemption, swaps, restore, proof checks
and WebSocket subscriptions. CDK's configuration applies each operation's setting
across **all payment methods**, not just the branch backend. Protecting swaps
also requires authentication when receiving ecash.

- Clear auth sends an identity-provider access token with each protected request.
- Blind auth uses one-use, denomination-one authorization proofs. A wallet signs
  in to obtain a batch, then spends a proof on each protected request. These are
  not monetary ecash and do not change a wallet's funds.
- The batch maximum is a per-request cap, not a per-user quota or cash limit.
  Account enrollment, access rules and sign-in methods belong to the identity
  provider. Disabling an account does not invalidate already issued blind proofs.
  Blind signatures prevent linking issuance to redemption cryptographically;
  timing and network metadata remain visible.

**Saving a draft does not change the mint.** Export the mint's full configuration
using `cdk-mintd --work-dir <mint-data> config export --file mint.toml`, replace
its `[auth]` block with Pecan's saved snippet, review the whole document, apply
it with `config apply --file mint.toml`, and restart mintd. Preserve existing
payment backends, key settings and management RPC configuration. Do not apply
an auth-only fragment as a complete configuration. Refresh policy in Pecan to
verify the change.

If quote creation or lookup is protected, choose **Connect processor**. Pecan
uses the *active* mint's OpenID discovery URL and client ID, displays the
provider's verification URL and user code, and waits for approval. Configure a
public OpenID client supporting device authorization; customer wallets may
use other flows offered by that provider. Pecan does not collect the provider's
password or host a customer wallet. Use HTTPS for the mint, discovery document,
and provider endpoints; plain HTTP is accepted only on loopback for local tests.

Pecan refreshes its session when available and obtains blind tokens on demand.
It verifies their DLEQ proofs with CDK, strips DLEQ information from redemption
headers, and removes each token from durable storage **before** dispatching the
request. Failed or interrupted requests therefore consume authorization tokens.
It does not retry monetary operations automatically. Tokens are bound to the
mint URL, discovery URL and client ID, and cannot follow a changed attachment.
Quote matching, settlement cross-checks and the Mint self-test all use this
connection. Rerun the self-test after connecting.

**Disconnect processor** deletes its locally stored credentials and cancels
pending sign-in. It does not revoke identity-provider sessions or sign customer
wallets out. The private `mint-auth.json` file in the processor config directory
contains access/refresh tokens and unused blind proofs; keep its directory and
backups private. On Unix the file is written with mode `0600`. Do not restore
an old copy of the blind-token pool: proofs may already have been consumed.
The existing Pecan access model is unchanged: all staff accounts can configure
and disconnect this shared processor connection.

### Wallet customer management (Keycloak)

**Access → Wallet customers** lists and searches customers in a dedicated
Keycloak realm. Operators can create an enabled account with a temporary
password, reset its password, disable or enable access, and sign it out.
Customers must change temporary passwords in Keycloak's browser sign-in flow.
No invitation email is sent. Passwords pass directly to Keycloak; Pecan does
not save them. Customer identities and credentials remain in Keycloak.

1. Create a **dedicated customer realm** in Keycloak (not `master`). Keep
   administrative users in `master`; all human accounts in the customer realm
   are manageable through Pecan. Turn off self-registration if only enrolled
   customers may use the mint. Keep realm backups alongside your other service
   backups; Pecan's backup does not include Keycloak.
2. Create the mint's **public wallet client** with authorization code + PKCE
   (S256), device authorization, and the wallets' redirect URLs, including
   `http://localhost:33388/callback`. Use signed JWT access tokens (RS256 or
   ES256). Put this realm's discovery URL and the public client ID in the mint's
   active auth configuration. Assign the `offline_access` optional client scope;
   Pecan requests it for background refresh. Pecan's device flow also sends PKCE.
3. Create a separate **confidential management client**, e.g. `pecan-management`,
   with service accounts enabled, browser/direct-access flows disabled, and
   `realm-management` service-account roles `query-users`, `view-users`, and
   `manage-users`. Do not grant `realm-admin`. This credential can administer
   users in the realm, so use a dedicated realm with no unrelated users.
4. Add these values to the installation's private `.env` (mode `0600`):

   ```dotenv
   KEYCLOAK_ISSUER=https://id.example.org/realms/mint-customers
   KEYCLOAK_CLIENT_ID=pecan-management
   KEYCLOAK_CLIENT_SECRET=<management-client-secret>
   ```

   Recreate the processor with the installation's Compose file to load them:
   `docker compose -f docker-compose.yml up -d --no-deps processor`.
   For a native binary, prefix these three names with `CDK_BRANCH_PROCESSOR_`.
   Set all three together; incomplete settings refuse startup. The service
   credential is never returned to the browser or included in `setup.json`.
5. Refresh **Wallet customers**. Pecan checks the mint's *active* discovery URL
   against the configured realm before every customer request. A saved draft
   is insufficient; a mismatch blocks customer management. HTTPS is required,
   except loopback HTTP for local tests. Pecan refuses provider redirects.

Every Pecan staff account can manage customers under the existing access model.
Customer accounts do not grant Pecan console/teller access. Pecan hides and
refuses to modify Keycloak service accounts. Changes are immediate; provider
errors stay visible and lists are paginated. Create and password-reset operations
must satisfy Keycloak's password/profile policies as well as Pecan's minimum
eight-character password requirement. Additional required profile fields are
completed by the customer in Keycloak. Email delivery, MFA enrollment, federation
and realm/client administration remain in Keycloak.

Disabling a customer blocks new sign-ins and refresh. Disable, reset-password
and sign-out actions end Keycloak sessions and revoke the wallet client's
offline refresh tokens. **Previously issued JWT access tokens remain valid
until expiry, and already issued blind tokens cannot be revoked by customer.**
Their use does not identify which account to disable. None of these actions
changes ecash balances or grants Pecan a per-customer transaction history. Use
short access-token lifetimes appropriate to the deployment. If a mutation's
sign-out step fails, Pecan reports the completed change and asks the operator to
retry **Sign out**, rather than claiming complete revocation.

See the [Keycloak Admin REST API](https://www.keycloak.org/docs-api/latest/rest-api/index.html)
and the reproducible local test in [`tests/auth/README.md`](../tests/auth/README.md#keycloak-customer-management).

Reproducible wallet compatibility checks and their limits are documented in
[`tests/auth/README.md`](../tests/auth/README.md).

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
| `processor-data` | branch ticket store (`tickets.json`), when the mint's keysets were first seen and stopped issuing (`keysets.json`), login sessions |
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
