# Pecan

**P**rocessor and **E**cash **C**onsole for **A**lternative **N**umeraires —
a cash counter for a [Cashu](https://cashu.space) mint. It lets a mint issue
ecash for a unit of your choice (a local currency, a voucher, a community
token), settled in person: the wallet creates a quote, the teller matches it
by the short code on the customer's screen, cash changes hands, done.

Two pieces: a **processor** that plugs into a
[cdk](https://github.com/cashubtc/cdk) mint over the stock payment-processor
interface, and a web **console** — a match-first teller page plus an operator
view with a live attachment checklist, an end-to-end self-test, and account
management.

## Install

One command on a Linux server (amd64/arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/zeugmaster/pecan/main/install.sh | bash
```

Run it as your normal user with access to Docker (including rootless Docker).
It installs into `~/pecan` and links `~/.local/bin/mintctl`; add
`~/.local/bin` to your `PATH` if needed. Root installs on Linux use
`/opt/pecan` and `/usr/local/bin/mintctl`. Use `--dir` to choose another
location. Docker Compose v2 with `up --wait-timeout` support is required.

The guided installer asks what this server should run:

- **Processor + a new mint** — pick a unit and the mint's hostname; the mint
  (official `cashubtc/mintd` image) is configured, connected, and verified by
  the time the installer finishes. Its seed is shown once — write it down.
- **Processor only** — attach a mint you already run afterwards, in the
  console's Mint tab. Pecan never touches that mint's seed, database, or
  keysets; it verifies the attachment and says exactly what to fix.

Then choose how the server is reached: a domain with automatic HTTPS (have
DNS A records ready — the installer shows exactly which), behind your own
reverse proxy, or plain HTTP on a trusted LAN. The console forces a password
change at the first sign-in. Every question has a flag twin for automation
(`mintctl install --help`):

```sh
mintctl install --yes --with-mint --unit ora \
  --console-domain console.example.org --mint-domain mint.example.org
```

## First steps

With a bundled mint: write down the seed, let the checklist confirm itself,
add teller accounts in the **Access** tab. Attaching your own mint: set unit
and mint URL in the **Mint** tab, apply the generated snippet to your mintd
(`cdk-mintd config apply`), restart it, and watch the checklist settle green.

## Day to day

```text
mintctl status | logs | update | domain | backup | restore | start | stop | uninstall
```

```sh
mintctl update --check          # preview a console update
mintctl update                 # update console + processor, keeping the mint pin
mintctl update --with-mint      # also use the release's tested mint version
mintctl update --mint-version <tag>  # update only the bundled mint
```

Updates show the version changes, ask for confirmation (`--yes` for
automation), and save a backup before applying them. Older installations can
use the bootstrap update command in the [operations guide](docs/operations.md#updates-and-recovery).

Backups, restore drills, server migration, proxy setups, and bundled-mint
operations live in [`docs/operations.md`](docs/operations.md).

## Good to know

- Needs cdk-mintd **v0.18.0-rc.0 or later** — the first release containing
  [cashubtc/cdk#2295](https://github.com/cashubtc/cdk/pull/2295). The
  protocol is checked at connect time and the console names the required
  release. Wallet developers: [`docs/wallet-integration.md`](docs/wallet-integration.md).
- Deposits are only accepted on wallet-locked quotes (NUT-20), and every
  settlement is cross-checked with the mint first.
- `mintctl backup` archives hold password hashes and the ticket ledger — and,
  with a bundled mint, its database and SEED. Store them encrypted.

## Development

`docker compose up --build` from a checkout — console on
http://localhost:9090 (demo `admin`/`admin`). Frontend in `web/`, processor
in `processor/`, installer in `mintctl/`; CI tests every PR and publishes
multi-arch images to `ghcr.io/zeugmaster/pecan`. The upstream research and
rescope record live in `docs/`.
