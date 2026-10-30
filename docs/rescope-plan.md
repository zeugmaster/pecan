# Re-scope plan: the attach-to-one-mint processor

Status: **implemented 2026-08-04** (same day; phases 0–3 in one pass — see the
repo state; the supply audit was dropped entirely per the follow-up decision).
Kept as the design record; §15's remaining open decisions: product naming
(deferred), full mTLS cert tooling (deferred).

Originally: draft for discussion, 2026-08-04. Companion to `docs/upstream-research/01..05`,
which document why the current design needed a patched cdk. This plan removes the need
for the patch by shrinking the product to what stock cdk supports today, with exactly one
upstream dependency: PR [cashubtc/cdk#2295](https://github.com/cashubtc/cdk/pull/2295)
(quote_id + NUT-20 pubkey on custom incoming quotes — our own PR, open, protocol
4.0.0, branch `zeugmaster/cdk#feat/custom-incoming-quote-id-pubkey`, head `f347804`).

---

## 1. Product re-definition

**Today** (PRODUCT.md): a browser-managed *mint lifecycle appliance*. The processor owns
the mint's config file, recovery seed, units, keysets, quote TTLs, advertisement, and
restarts; cdk-mintd is a managed subordinate built from a 5-hunk patch.

**Target**: a *payment backend + teller console that attaches to one existing cdk-mintd*.

> Run a cash counter for a Cashu mint you already operate. Point your mint's
> `[grpc_processor]` at this processor, and tellers settle deposits and payouts by
> matching the quote id off the customer's wallet.

The processor never writes mint configuration. Instead it **takes the mint as
configured**, continuously **checks** that the mint is set up for branch settlement with
the configured unit, and **communicates** precisely what to fix when it is not — via a
live checklist, a copyable config snippet, and an end-to-end self-test.

Scope changes per research doc:

| Doc | Constraint | Decision |
|---|---|---|
| 01 | gRPC settings carry a single unit; one processor ↔ one unit | **Accepted.** The processor serves exactly one unit, reported verbatim in `get_settings`. The stock boot handshake (`validate_backend_unit`) passes untouched. |
| 02 | Stock cdk never passes quote_id/pubkey on custom mint quotes | **Solved upstream** by PR #2295. The extra_json injection hunk dies; `backend.rs` reads the new first-class fields. Until merge, both sides build from the PR branch. |
| 03 | Mint-info pinned to first RPC-enabled boot; advertisement unmanageable | **Management removed.** The processor no longer tries to make `/v1/info` follow config — it *detects* stale/missing advertisement and shows the operator the remedy (see §7, check `advertised`). |
| 04 | First keyset shape unconfigurable | **Management removed.** Keysets are the mint operator's concern; the console shows the unit's keysets read-only. The default powers-of-2/no-expiry keyset is acceptable. |
| 05 | No seed↔DB guard in stock cdk | **Moot for us.** The processor no longer generates, stores, or reveals a mnemonic — there is no seed in this product to guard. (The upstream seed-guard PR from doc 05 is still worth opening independently; it protects mint operators.) |

Explicitly out of scope after the re-scope (with return paths in §16): multi-unit,
unit lifecycle (redemption-only/retired), keyset rotation + rollover worker, quote-TTL
sync, mint identity editing, supply audit, mnemonic custody/backup, mint restarts, the
bundled `mint` container and its standby supervisor, the management RPC entirely.

What stays untouched: the teller flow and ticket state machine (`processor/src/state.rs`
— quote-id matching, waiting/pending/paid/failed, replay protection, expiry sweep, open
cap), users + sessions + forced first password change, the mint cross-check before
settlement (`verify_with_mint`), the design system (DESIGN.md, strict grayscale shadcn),
`/healthz`, SSE-driven refresh.

---

## 2. Compatibility contract

The payment-processor protocol check is **strict string equality** on
`x-cdk-protocol-version`. PR #2295 bumps it to `4.0.0` (required field, per the #1973
convention). Consequences:

| mintd build | Result against re-scoped processor |
|---|---|
| cdk ≤ 0.17.3 (protocol 3.0.0) | Connect rejected at the interceptor. Checklist shows "mint has never attached" + guidance naming the required build. |
| `zeugmaster/cdk@f347804` (PR branch) | Full function. This is the supported pairing until merge. |
| cdk main after #2295 merges | Full function; we repin to the merge commit / next release. |

Plan of record:

1. **Now**: pin `processor/Cargo.toml` (cdk-common, cdk-payment-processor) to
   `git = "https://github.com/zeugmaster/cdk", rev = "f3478044…"`. Drop `cdk-mint-rpc`
   (no management RPC anymore). Ship a clearly-temporary `docker/mintd/Dockerfile`
   building stock `cdk-mintd` from the same rev — no patch — for operators and the e2e
   rig, until an upstream release contains #2295.
2. **On merge**: repin to the cashubtc/cdk merge commit; on release, to the tag. The
   snippet's "build your mintd from …" line is generated from a single
   `COMPATIBLE_CDK_REV` constant so it can never drift from the actual pin.

Known API drift to absorb at repin (v0.17.2 → main): `CustomIncomingPaymentOptions.amount`
is now `Option<Amount<CurrencyUnit>>` (upstream #2146); `#2275` may add a `method` field
(field 5 — our PR deliberately took 6/7); verify `WaitPaymentResponse`/event shapes
against the accounting refactor (`04cbe81c`). None affect the ticket model.

*Executed 2026-08-18: #2295 merged 2026-08-11 and shipped in cdk **v0.18.0-rc.0**;
repinned to crates.io `=0.18.0-rc.0` (zero code drift — the PR-head pin already matched
the merged API), `COMPATIBLE_CDK_REV` became `COMPATIBLE_CDK_VERSION`,
`docker/mintd/Dockerfile` deleted; operators run the official
`cashubtc/mintd` image at version 0.18.0-rc.0. The 0.18 config model (database-authoritative,
`config init`/`config apply`, `[[ln]]` → `[[payment_backend]]`) is reflected in the
snippet renderer and checklist remedies.*

---

## 3. Target architecture

One container, one binary, three jobs:

```
┌────────────────────────────────────────────────────────┐
│ cdk-branch-processor                                   │
│                                                        │
│  gRPC :50051  ← the operator's cdk-mintd connects in   │
│    MintPayment for method "branch", single unit        │
│                                                        │
│  HTTP :9090   ← operators/tellers                      │
│    console SPA + JSON API + SSE + /healthz             │
│                                                        │
│  outbound HTTP → the mint's public API (mint_url)      │
│    /v1/info · /v1/keysets · quote lookups · self-test  │
└────────────────────────────────────────────────────────┘
```

- **No management RPC client, no sqlite read, no mint.toml, no supervisor.** The only
  channels to the mint are: inbound gRPC (the mint calls us) and the mint's public
  wallet-facing HTTP API (we call it — same URL wallets use, which is itself a check).
- **gRPC transport**: mintd main refuses plaintext unless `[grpc_processor]
  allow_insecure = true` (verified on the PR branch, `setup.rs:326-338`). Default
  posture stays "plaintext on a private network, stated loudly", so the snippet emits
  `allow_insecure = true`. Additionally, expose the server's existing mTLS support
  (`PaymentProcessorServer::start(Some(tls_dir))` expects `server.pem`/`server.key`/
  `ca.pem` and verifies client certs) via a new `CDK_BRANCH_PROCESSOR_TLS_DIR` env var;
  when set, the snippet switches to `tls_dir = "…"`. Certificate *generation* stays out
  of scope (documented openssl recipe; a `mintctl` helper is deferred).

### Files and volumes

| Path (volume) | Contents | Change |
|---|---|---|
| `config-data:/var/lib/custom-unit-mint/config` | `setup.json` (v4), `users.json` | `mint.toml` no longer written; volume no longer mounted anywhere else |
| `processor-data:/var/lib/cdk-branch-processor` | `tickets.json`, `sessions.json` | unchanged; `managed-stack-backup.json` retired (it existed to protect the seed) |
| `mint-data` | — | **gone** (was the bundled mint's DB) |

### Environment surface

Kept: `WORK_DIR`, `CONFIG_DIR`, `GRPC_ADDR`, `GRPC_PORT`, `HTTP_ADDR`, `HTTP_PORT`,
`INITIAL_ADMIN_PASSWORD`, `VERSION`, `WEB_DIST`. New: `TLS_DIR` (optional).
Removed: `MINT_RPC_URL`, `MINT_HTTP_URL`, `DEFAULT_MINT_PUBLIC_URL`, `MINT_GRPC_ADDR`,
`MINT_DB_PATH`, `MINT_MODE` (`processor/src/main.rs:54-74`). The mint URL becomes
console-owned config, not deployment topology.

---

## 4. Configuration model (setup.json v4)

```json
{
  "version": 4,
  "configured_at": 1754300000,
  "method": "branch",
  "unit": "ora",
  "mint": {
    "url": "https://mint.example.org",
    "advertised_grpc": "http://10.0.0.5:50051"
  },
  "unit_locked": true
}
```

- `unit`: empty until first-run setup; the single unit this install serves. Same slug
  rule as today (`[a-z0-9_-]+`). Must match the mint's `[[ln]] unit` byte-for-byte —
  guaranteed by generating the snippet from this value.
- `mint.url`: the mint's public HTTP base — used for all checks, the teller cross-check,
  and the self-test. Empty = unattached.
- `mint.advertised_grpc`: how the mint reaches *us*; only used to render the snippet.
- `unit_locked`: set on the first successful self-test (or first settled ticket).
  A locked unit is read-only in the console (§8); changing it afterwards is a
  documented manual edit, because issued ecash and open quotes reference it.
- Gone: `mnemonic`, `seed_fingerprint`, `auth` (already migrated), `endpoints` block,
  `rollover`, `units[]`, `mint_connection`.

**Migration v3 → v4** (in `AppConfig::upgrade`):

1. Rename the old file to `setup.json.v3-managed.bak` **before** writing v4 — it contains
   the mnemonic and must never be destroyed silently. `mint.toml` in the config dir is
   left in place untouched (a bundled mint keeps running from it; it now belongs to the
   operator).
2. Map: `unit` ← `mint.unit` (the primary); `mint.url` ← External `http_url`, else
   `endpoints.public_url`; `advertised_grpc` ← External value, else
   `endpoints.processor_grpc_addr:port`.
3. Multi-unit v3 configs: adopt the primary unit, log the dropped ones; their historical
   tickets remain visible (tickets carry their unit string), and still-open tickets of
   other units can still be settled — only *new* quotes are single-unit.
4. Console shows a one-time "this install previously managed its mint" notice naming the
   backup file and stating that the recovery phrase inside it now belongs to whoever
   operates the mint.

`users.json`, `sessions.json`, `tickets.json` migrate untouched.

---

## 5. Payment backend changes (`processor/src/backend.rs`)

- **Single unit.** `units: HashMap<CurrencyUnit, UnitLifecycle>` + `primary_unit`
  collapse to one `ArcSwap<Option<CurrencyUnit>>` (or `RwLock`) shared with the web
  layer, so first-run setup applies **live** — no process restart, which deletes the
  whole exit(0)/restart-toast machinery (§8). Lifecycle gating (`can_mint`/`can_melt`)
  disappears; an unconfigured unit rejects everything with
  `"processor not set up yet — finish setup in the console at …"` so the mintd log says
  why its boot failed.
- **`get_settings`** reports the configured unit verbatim (satisfies the stock
  handshake, doc 01) and `custom = {"branch": "{}"}` as today. The "wire filler" comment
  era ends.
- **Incoming quotes** read `opts.quote_id: QuoteId` and `opts.pubkey: Option<PublicKey>`
  directly (PR #2295 shape). `incoming_meta`/`QUOTE_ID_FIELD`/`PUBKEY_FIELD` and the
  extra_json parsing for them are deleted; the pubkey-required policy stays (branch
  deposits must be NUT-20-locked), now enforced on a typed field. `extra_object` stays
  only for the melt `amount` field. `opts.amount` is `Option` on main → explicit error
  when absent.
- **Melt path unchanged** (outgoing quote_id has been stock since 0.17).
- **New instrumentation** for the checklist (§7): timestamps for last `get_settings`
  call, stream attach (exists as `payment_stream_attached`), and last event delivered;
  attach transitions fire `notify_ui_change` so open consoles settle live.

`state.rs` is untouched except one stale doc comment (melt tickets' expiry is no longer
"the TTL this processor configures on the mint" — it's a local bookkeeping window).

---

## 6. What the operator's mintd needs (the snippet)

Generated from config (unit, advertised_grpc, TLS mode) and rendered in the console with
a copy button — replacing today's `render_external_mint_snippet`:

```toml
# Branch settlement backend — merge into your cdk-mintd mint.toml.
# Requires cdk-mintd built from <COMPATIBLE_CDK_REV> (payment-processor
# protocol 4.0.0; PR cashubtc/cdk#2295).

[[ln]]
ln_backend = "grpcprocessor"
unit = "ora"
min_mint = 1
max_mint = 500000
min_melt = 1
max_melt = 500000

[grpc_processor]
supported_units = ["ora"]          # required non-empty on current cdk main
address = "10.0.0.5"
port = 50051
allow_insecure = true              # gRPC is plaintext: private network only
# …or, if the processor has CDK_BRANCH_PROCESSOR_TLS_DIR set:
# tls_dir = "/path/to/certs"       # ca.pem + client cert/key

# Fresh databases only — seeds counter-friendly quote lifetimes.
# (60 s default melt TTL is too short for a counter visit; see checklist.)
[info.quote_ttl]
mint_ttl = 1800
melt_ttl = 900
```

Deliberately absent versus today's snippet: `unit_keysets` blocks (patch-only concept),
`[mint_management_rpc]`, multiple `[[ln]]` entries, the "patch applied" requirement.
Limits are the operator's to tune; the values above are commented defaults.

---

## 7. The attachment checklist (core of the new UX)

Server-side, computed into `/api/app` (and refreshable on demand), five checks. Each
renders per DESIGN.md rules — icon + text label, never color — with states **ok / warn /
fail / unknown** and a remedy block (plain language + copyable commands) when not ok.

| # | id | Plain-language question | Signal | Failure remedies shown |
|---|---|---|---|---|
| 1 | `reachable` | "Is the mint online?" | `GET {mint_url}/v1/info` (2 s timeout); captures name, version, icon for the identity card | Wrong URL / mint down / TLS errors, verbatim transport error shown |
| 2 | `advertised` | "Does the mint accept branch payments for ORA?" | `/v1/info` `nuts.4.methods` and `nuts.5.methods` contain `(ora, branch)` | (a) snippet not applied → show snippet; (b) **applied but pinned** (doc 03: RPC-enabled mints pin `/v1/info` to first boot): explain, offer two fixes — disable `[mint_management_rpc]` and restart (config then wins every boot), or run the `cdk-mint-cli` NUT-04/05 update commands (exact invocations, unit/method prefilled — CLI syntax confirmed at implementation); (c) mint-only or melt-only advertised → warn with the missing half; (d) mint advertises branch for *other* units too → warn "this processor only serves ora; quotes for X will fail" |
| 3 | `attached` | "Is the mint connected to this processor?" | last `get_settings` timestamp + payment-event stream attach (backend instrumentation, §5) | Never attached → "check `[grpc_processor]` address/port, network, and that your mintd is built from <rev> — protocol 4.0.0 mismatches are rejected at connect (visible in the mintd log)". Settings-seen-but-no-stream → "mintd started the handshake but did not finish — check its log for a unit mismatch" |
| 4 | `keyset` | "Does the unit have active keys?" | `/v1/keysets` has an active keyset for the unit | "The mint creates keys for ora on its first start with the `[[ln]]` entry — restart your mintd." Read-only display of id/fee/expiry (no rotation controls — doc 04 accepted) |
| 5 | `end_to_end` | "Do deposits reach the counter?" | Last **self-test** result (below), with timestamp | Failure taxonomy below |

**Self-test** (`POST /api/mint/self-test`, operator-triggered button; auto-run once when
check 3 first turns ok): the processor acts as a wallet for one round-trip —

1. Generate an ephemeral NUT-20 keypair (via `cdk-common`'s re-exported cashu types).
2. `POST {mint_url}/v1/mint/quote/branch` with `{amount: 1, unit, pubkey,
   description: "Connection self-test — safe to ignore"}`.
3. The mint calls our `create_incoming_payment_request` over gRPC; the response gives us
   the mint's `quote` id → confirm the matching ticket `MINT-<quote_id>` exists locally,
   then void it immediately with note `self-test` (filtered out of the open list, the
   activity feed, and teller matching).
4. Record: quote_id + pubkey arrived (PR #2295 working), observed quote expiry → derived
   mint-quote TTL, round-trip latency.

Failure taxonomy — each mapped to one message:

| Observation | Meaning shown to the operator |
|---|---|
| HTTP error / timeout on step 2 | Mint unreachable (same as check 1) |
| 400/404: method or unit unknown | Snippet not applied / advertisement pinned → check 2 remedies |
| 5xx from the mint | Mint→processor gRPC hop failed — address/TLS/version; "check the mintd log" |
| Quote created but **no local ticket** | The mint is pointed at a *different* processor instance |
| Quote expiry < ~10 min | Warn: "deposit quotes expire after N — tight for a counter visit; raise `[info.quote_ttl]`" (melt TTL warning worded likewise) |
| All good | "Verified end to end" + timestamp |

The self-test leaves one unpaid quote at the mint per run (expires harmlessly); noted in
the button's description. This one probe validates the entire chain — mint reachability,
advertisement, gRPC attachment, PR fields, TTLs — without needing a customer wallet.

---

## 8. Console changes

Design system, shell, teller, login, and auth flows stay exactly as they are
(`DESIGN.md` rules all still hold; no new colors, no new component primitives).

**Tabs: `Overview | Mint | Access`** (Units tab deleted; hash deep-links unchanged).

| Surface | Change |
|---|---|
| **Overview** | Health tiles become: *Mint* (check 1), *Payment link* (check 3), *Setup* (worst-of-checklist summary, links to Mint tab), *Open quotes*, *Net settled* (teller ledger — already computed as `summary.net_issued`). The circulation/supply tiles and the unit-balances table go with the supply audit; the settled-activity chart **stays** (it is ledger-derived, not audit-derived). |
| **Mint tab** (rebuilt) | Cards, top to bottom: **Attachment** (mint URL + advertised gRPC; guarded change flow), **Checklist** (§7, five rows + refresh), **Mint config snippet** (§6, copy button), **Self-test** (button + last result), **Mint identity** — *read-only* facts from `/v1/info` (name, version, description; replaces the editable IdentityCard — the mint operator owns identity now), **Keysets** — read-only table for the unit. Deleted: bundled/external mode switcher, RecoveryCard (mnemonic), supply-audit availability rows. |
| **Access** | Unchanged. |
| **Teller** | Unchanged (self-test tickets filtered out before it ever sees them). |
| **Removed frontend files** | `units-tab.tsx`, `unit-dialogs.tsx`, `lifecycle-actions.tsx`, `lib/restart.ts` (no mutation restarts the process anymore — all config applies live), supply portions of `overview-tab.tsx`. |

**First-run journey** (empty states, no wizard — consistent with today's philosophy):

1. Sign in (`admin` + installer passphrase) → forced password change (unchanged).
2. Overview empty state: "Not attached to a mint yet — set up in the Mint tab."
3. Mint tab, Attachment card in setup mode: **unit** + **mint URL** + advertised gRPC
   (prefilled `http://<host>:50051`). Saving applies live — no restart.
4. Checklist appears, initially failing; snippet card is now populated. Operator merges
   the snippet into their mint.toml, restarts *their* mintd.
5. Checks 1–4 settle live via SSE as the mint boots and attaches; self-test runs; check 5
   turns ok → banner "Ready for the counter", link to the Teller.
6. Unit locks (`unit_locked`) on first success; thereafter the Attachment card renders
   the unit as a fact, not a field.

---

## 9. HTTP API delta

| Endpoint | Fate |
|---|---|
| `GET /healthz`, `GET /api/app`, `GET /events`, SPA/assets/fonts | keep |
| `POST /api/login` `/api/logout` `/api/me/password`, users CRUD | keep |
| `POST /api/quotes/match`, `/api/tickets/{id}/mark-paid`, `mark-failed` | keep |
| `POST /api/units`, `/api/units/{u}/lifecycle`, `/api/units/{u}/policy` | **delete** |
| `POST /api/keysets/rotate` | **delete** |
| `POST /api/settings/identity`, `/api/settings/mnemonic` | **delete** |
| `POST /api/settings/mint-connection` | **replace** with `POST /api/settings/attachment` `{unit?, mint_url, advertised_grpc}` (unit accepted only while unlocked) |
| `POST /api/mint/self-test` | **new** |

`/api/app` snapshot: drop `supply`, `units[]`, `unit_summaries`, `mint_connection`
modes, rollover fields, `endpoints` block; add `attachment`, `checklist[]`,
`self_test`, `mint_identity` (name/version/description from `/v1/info`), `keysets[]`.
Frontend `lib/api.ts` types follow.

---

## 10. Code inventory (backend)

| File | Action |
|---|---|
| `config.rs` (1134) | Shrink to ~350: v4 schema + migration, slug/URL validation, password hashing (unchanged). Delete: mnemonic gen + fingerprint, `render_mint_toml`, `unit_backend_blocks`, external snippet (moves to a small `snippet.rs` in the new shape), `RolloverPolicy`, `ManagedUnit`, `UnitLifecycle`, `MintConnection`, `EndpointConfig`, backup-path plumbing |
| `main.rs` (468) | Shrink to ~220: drop rollover worker, quote-TTL sync, mint-db path, mint-mode env; keep sweeper, gRPC + HTTP serve; pass `TLS_DIR` into `server.start(...)` |
| `clients.rs` (242) | Delete `MintRpcClient`; keep `MintHttpClient` (+ `get_info` capturing identity fields, + self-test request helper). Quote lookups for the teller cross-check unchanged |
| `backend.rs` (393) | §5 changes; net smaller |
| `supply.rs` (257) | **Delete** (bundled-only by design; feature leaves with the bundled mint) |
| `state.rs`, `users.rs`, `sessions.rs` | Keep (comment touch-ups only) |
| `web.rs` (2079) | Delete ~6 handler groups (§9), add checklist assembly + self-test (~250); net ~1500 |
| `patches/` | **Delete** |
| `scripts/mint-supervisor.sh`, `mint-health.sh` | **Delete** |
| new `checks.rs` | Checklist evaluation + self-test orchestration, unit-testable against a fake mint HTTP server |

---

## 11. Deployment & installer

**Dockerfile**: drop the cdk-mintd build stage and patch application entirely — the image
contains only the processor + web dist (build time and image size drop sharply; the
`patch-check` CI job dies). Keep the GHCR publish workflow. A separate, clearly-temporary
`docker/mintd/Dockerfile` builds stock mintd from `COMPATIBLE_CDK_REV` for the e2e rig
and for operators who need a #2295-capable mintd before an upstream release exists.

**docker-compose.yml**: services `processor` (+ `caddy` under the existing `tls` profile
for the console domain). The `mint` service, `mint-data` volume, and the shared
`config-data` read-only mount into the mint are removed. gRPC port publishing unchanged
(`127.0.0.1` vs `0.0.0.0` choice survives).

**mintctl wizard** becomes processor-only:

1. Release/platform + Docker preflight (unchanged)
2. Existing-install check (unchanged)
3. Console port conflict check (mint port check gone)
4. "Where does your cdk-mintd run?" → gRPC bind `127.0.0.1` vs `0.0.0.0` + firewall
   warning (today's processor-only follow-up, promoted; the bundled/processor-only
   question is deleted)
5. Console reachability: domain+TLS / behind-proxy / plain HTTP (unchanged, console only
   — the mint domain/`MINT_PUBLIC_URL` questions are gone)
6. Admin passphrase generation, install, `/healthz` wait (unchanged)
7. Finish screen: console URL + credentials + "next: open the Mint tab and follow the
   checklist" (replaces "reveal and back up the recovery phrase")

`.env` shrinks accordingly (`MINT_PORT`, `MINT_PUBLIC_URL`, `MINT_MODE` gone).
`mintctl status` drops the supervisor-state read; `backup`/`restore` cover
`processor-data` + `config-data` + `.env` and the copy stops claiming the archive
contains a recovery seed — it holds tickets, users, sessions, attachment config.
`mintctl update` unchanged. Existing bundled installs: `mintctl update` refuses with a
pointer to a short migration note (the old compose keeps a mint service the new one
doesn't know about); this is acceptable pre-1.0, and the note covers keeping their
mintd running standalone with the last generated `mint.toml`.

---

## 12. Documentation

- **PRODUCT.md**: purpose rewritten around §1; users become "operators of an existing
  cdk mint who want person-present cash settlement".
- **DESIGN.md**: console is `Overview | Mint | Access`; delete the restart-aware
  mutations section (nothing restarts), the recovery-phrase dialog reference, units/
  lifecycle dialog examples; the teller section is untouched.
- **README.md**: rewrite positioning + quickstart ("have a mint → install processor →
  paste snippet → self-test"); compatibility section replaces the patch/CDK-rev section,
  stating the #2295 dependency and the temporary mintd image.
- **docs/operations.md**: backup/restore (no seed language), no supervisor/standby
  section, no keyset lifecycle; add "the mint is yours: its seed, database, backups, and
  keysets are managed by you/upstream tooling".
- **docs/upstream-research/**: stays as the upstream agenda. Add a short README noting
  which constraints the re-scope retired (03/04/05 for this product) and which remain
  live asks (02 = #2295 in flight; 05 recommended as a standalone contribution; 01 =
  future multi-unit).

---

## 13. Testing

- **Unit**: config v4 + v3 migration (mnemonic-preserving rename, multi-unit adoption);
  checklist evaluation against a mocked mint (each state/remedy); self-test correlation
  + void; backend single-unit gates + new field consumption; snippet golden test.
- **e2e (docker rig)**: mintd built from `COMPATIBLE_CDK_REV` + processor. Scenarios:
  fresh attach happy path (setup → snippet → attach → self-test green → wallet deposit →
  teller settle → melt payout); stock-0.17 mintd → checklist stuck at "never attached"
  with the version message; advertisement-pinned mint (RPC-enabled, entry added late) →
  check 2 fail + remedy; wrong-processor scenario (quote without local ticket).
- **Installer e2e**: existing mintctl recipe re-recorded for the shorter wizard.
- CI: drop `patch-check`; add the mintd-image build to the rig job only (not to publish).

---

## 14. Phasing

| Phase | Content | Size |
|---|---|---|
| 0 — groundwork | Repin to `f347804`, absorb API drift, backend reads new fields, delete patch + mintd build from the image, temporary mintd Dockerfile, e2e rig boots green | M |
| 1 — de-manage | Config v4 + migration; delete rollover/TTL/RPC/supply/supervisor/restart machinery; single-unit backend with live-apply; API deletions | L |
| 2 — attachment UX | `checks.rs`, instrumentation, self-test, snippet; Mint tab rebuild, Overview reshape, first-run journey; SSE settle | L |
| 3 — deployment | Dockerfile/compose/mintctl/env/docs; backup-restore rescope; migration note for bundled installs | M |
| 4 — hardening | TLS_DIR passthrough + docs, full e2e matrix, copy polish, `unit_locked` edge cases | M |

Each phase leaves the repo shippable; 0→1 order matters (compile against the new pin
before deleting the old model), 2 and 3 can interleave.

---

## 15. Decisions to confirm (recommendations inline)

1. **Supply audit**: removed with the bundled mint (recommended — it reads the mint's
   sqlite, impossible for a genuinely external mint, and its schema coupling was the
   price of the old architecture). Alternative: keep behind an optional
   `MINT_DB_PATH` for co-located installs — not recommended (drifting schema on cdk
   main, contradicts "the mint is not ours").
2. **Naming**: repo/image "custom-unit-mint" now oversells (it is no longer a mint).
   Recommend keeping repo/image names for now (churn, GHCR continuity) and re-branding
   the console/product copy to e.g. "Branch Console"; rename decision deferred.
   *Resolved 2026-08-17: renamed to **pecan** (Processor and Ecash Console for
   Alternative Numeraires) — repo, image, install paths, and product copy.*
3. **One-box convenience**: no compose-managed mint service (recommended); docs show a
   worked example running the temporary mintd image beside the processor, attached like
   any external mint.
   *Revisited 2026-08-18: a compose-managed mint returns as an opt-in installer mode
   (`--with-mint`). The 2026-08 objection was the temporary fork-built image; with
   #2295 shipped in the stock `cashubtc/mintd` image at version 0.18.0-rc.0 that objection is
   gone. The mint stays stock and self-contained — installer-written import document,
   seed shown once + carried in backups, image pinned independently of pecan releases;
   the console still only verifies. Processor side: the `CDK_BRANCH_PROCESSOR_INITIAL_*`
   first-boot attachment envs.*
4. **TLS default**: `allow_insecure = true` + private-network warning as the default
   posture, `TLS_DIR` supported for operators who bring certs (recommended). Full mTLS
   tooling deferred.
5. **Self-test auto-run**: once, when check 3 first turns green (recommended), plus the
   manual button. Pure-manual is the conservative fallback.

---

## 16. Future / return paths

| Removed now | Comes back when |
|---|---|
| Multi-unit | Upstream accepts a `repeated units` settings field (doc 01's ask) — backend re-grows a unit set; checklist checks per-unit advertisement |
| Advertisement management | #2242 (config-in-DB, `config apply`) or doc 03's membership-follows-config fix lands — the checklist's *remedy* could become a *button* |
| Keyset shaping/rotation | Doc 04's `[keyset.<unit>]` config or #2253 auto-rotation lands upstream — likely stays the mint operator's tool, surfaced read-only here |
| Seed guard | Doc 05's PR is mint-side; open it upstream independently of this product |
| Supply audit | Only via an upstream reporting API (never again via sqlite) |
