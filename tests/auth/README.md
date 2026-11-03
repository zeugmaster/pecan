# Wallet authentication interoperability

This suite runs **real CDK 0.18.2 mintd/CLI binaries** and **Nutshell 0.20.2**
against a local test identity provider. It also exercises Pecan through its API
and Chromium UI. No production services or funds are used. The provider
intentionally auto-approves test identities; never deploy it.

## Run

Build the current Pecan checkout:

```sh
npm --prefix web ci
npm --prefix web run build
cargo build --manifest-path processor/Cargo.toml --locked
```

Put the official `cdk-cli` and `cdk-mintd` **0.18.2** release binaries for your
platform in `/tmp/pecan-auth-tools/`, verify them against the release's
`SHA256SUMS`, and make them executable. Downloads are available from
[the CDK release](https://github.com/cashubtc/cdk/releases/tag/v0.18.2).

```sh
uv venv /tmp/pecan-auth-tools/venv --python 3.12
uv pip install --python /tmp/pecan-auth-tools/venv/bin/python \
  'cashu==0.20.2' 'marshmallow==3.26.2' 'playwright==1.64.1'
/tmp/pecan-auth-tools/venv/bin/playwright install chromium
/tmp/pecan-auth-tools/venv/bin/python tests/auth/interop.py
```

`AUTH_TEST_TOOLS` can override the tool directory. Nutshell's `environs`
dependency needs Marshmallow 3; an unconstrained installation currently picks
an incompatible Marshmallow 4. No installed wallet source is patched.

The suite refuses occupied ports **18085, 18086, 18090, 19090, 15051**. It uses
a fresh private temporary directory for every run, starts a separate branch
mint and fake-Lightning mint (CDK forbids mixing those backends), and stops only
its own processes. Logs, screenshots and `results.json` remain in the printed
temporary directory. Logs and databases contain test credentials and tokens;
do not publish them. Existing wallets, seeds and running Pecan services are
not used. The identity provider supports device and password grants, including
one pending poll and refresh-token rotation.

## Observed compatibility

Verified on 2026-10-27 with the macOS ARM64 release binaries.

| Client | Login | Blind auth | Direct clear auth on quote endpoints |
| --- | --- | --- | --- |
| Pecan working tree | Device code | Supported | Supported |
| Nutshell 0.20.2 CLI | Password grant tested | Minting, send/swap, receive and Lightning withdrawal tested | Fails with `KeyError: 'clear_auth_token'` |
| CDK CLI 0.18.2 release binary | Device code succeeds | `mint-blind-auth` fails with `Auth settings undefined`; `mint` fails with `Blind Auth Required` | `mint` fails with `Clear Auth Required` |

Nutshell successfully supplies a clear token to **issue blind tokens**. That
does not imply it supplies clear tokens to every payment endpoint. Its CLI
currently selects authorization-code login by default, or password login with
`auth --password`; the device-flow implementation is not selected by that
command. CDK's CLI login succeeds, but its payment commands do not establish
usable auth state in this tested release. The suite records these exact known
failures; different errors fail the suite. A successful suite means Pecan's
checks pass and these wallet limitations were either reproduced or resolved,
not that every CLI operation succeeded.

Useful CLI commands (point them only at the isolated mint):

```sh
cdk-cli -w /tmp/test-cdk cat-device-login http://127.0.0.1:18086
cdk-cli -w /tmp/test-cdk mint-blind-auth http://127.0.0.1:18086 32
CASHU_DIR=/tmp/test-nutshell cashu --host http://127.0.0.1:18086 auth --mint --password
```

## What is checked

- Auth routes require Pecan login; saving a draft does not change live policy.
- UI-generated configuration is accepted by stock CDK and advertised correctly,
  including disabling auth and restoring open quote operations.
- Processor device login, pending polling, refresh, automatic BAT issuance,
  private-file permissions, restart persistence and disconnect.
- Protected branch deposit/withdrawal quote probes, teller lookup and settlement.
  A settlement after disconnect is refused until the processor reconnects.
- A branch deposit is also created with Nutshell's auth **library** and raw
  protocol requests; a spent BAT is rejected. This is not a claim that Nutshell's
  CLI supports Pecan's custom method or that a full custom-method withdrawal was
  completed by a CLI wallet.
- Nutshell's real CLI blind-auth issuance, deposits, send/swap, receive and
  fake-Lightning withdrawal; the CDK CLI commands above and direct clear auth
  on both wallets.
- Desktop/mobile Access UI, device-code sign-in, draft saving, distinguishing
  active policy from saved policy, and disconnect.

Rust unit tests additionally cover route matching for custom methods, URL and
policy validation, token binding, one-use persistence and credential removal.

Production identity providers, TLS deployments, WebSocket authentication,
wallet restore and a full custom-method cash withdrawal are not covered by this
suite. The UI exposes their CDK policy controls; verify your chosen wallets and
provider before enabling them. The tests leave WebSocket authentication open
so HTTP authentication compatibility can be assessed independently.
