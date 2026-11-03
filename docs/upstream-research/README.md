# Upstream research

Five investigations (2026-07/08) into why the original, mint-managing design
could not run on stock cdk, each ending in a concrete upstream ask. After the
re-scope to an attach-to-one-mint processor (`docs/rescope-plan.md`), their
status:

| Doc | Constraint | Status after the re-scope |
|---|---|---|
| 01 | One unit per gRPC processor (settings handshake) | **Accepted as product shape** — one unit per install. The `repeated units` proposal remains the path to multi-unit, unproposed so far. |
| 02 | Quote id / NUT-20 pubkey not passed to custom backends | **Became [cashubtc/cdk#2295](https://github.com/cashubtc/cdk/pull/2295) — merged 2026-08-11, released in cdk v0.18.0-rc.0.** The dependency is a plain crates.io release pin; see `processor/Cargo.toml` for the current tested version. Nothing fork-hosted remains. |
| 03 | Mint info pinned to first RPC-enabled boot | **No longer patched.** The constraint still exists upstream and now surfaces as operator guidance: the console's "advertised" check detects the pinning and shows the remedy. Doc 03's fix proposal (membership-follows-config + dedup) is still worth filing. |
| 04 | Initial keyset shape unconfigurable | **Out of scope** — keysets are the mint operator's concern; the console shows them read-only. The `[keyset.<unit>]` config proposal is still worth filing for mint operators. |
| 05 | No seed↔DB guard in the signatory | **Out of scope for this product** (no seed custody anymore), but the small upstream PR remains a good contribution for every mint operator — see doc 05 §7. |

The documents are kept verbatim as written (they cite the pre-rescope
codebase, e.g. `patches/cdk-managed-units.patch`, which no longer exists);
read them as the record of *why* and as the agenda for future upstream work.
