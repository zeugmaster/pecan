---
name: Pecan
description: Monochrome operator console and teller for the branch payment processor — settlement, mint-attachment verification, and access operations.
colors:
  background: "oklch(0.985 0 0)"
  card: "oklch(1 0 0)"
  foreground: "oklch(0.145 0 0)"
  primary: "oklch(0.205 0 0)"
  primary-foreground: "oklch(0.985 0 0)"
  secondary: "oklch(0.97 0 0)"
  muted: "oklch(0.97 0 0)"
  muted-foreground: "oklch(0.556 0 0)"
  accent: "oklch(0.97 0 0)"
  border: "oklch(0.922 0 0)"
  input: "oklch(0.922 0 0)"
  ring: "oklch(0.708 0 0)"
  dark-background: "oklch(0.145 0 0)"
  dark-card: "oklch(0.205 0 0)"
  dark-foreground: "oklch(0.985 0 0)"
  dark-primary: "oklch(0.922 0 0)"
  dark-primary-foreground: "oklch(0.205 0 0)"
  dark-border: "oklch(1 0 0 / 10%)"
typography:
  headline:
    fontFamily: "Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "24px"
    fontWeight: 600
    lineHeight: 1.2
  title:
    fontFamily: "Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 600
    lineHeight: 1.5
  body:
    fontFamily: "Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: 1.5
  mono:
    fontFamily: "JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, monospace"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  sm: "6px"
  md: "8px"
  lg: "10px"
  xl: "14px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
---

# Design System: Pecan

## 1. Overview

**Creative North Star: "The Monochrome Till"**

Pecan is trustworthy infrastructure software rendered in strict
grayscale. It is a stock shadcn (new-york, neutral base) interface with every
color removed: no blue accent, no red danger, no green success. State and
intent are carried by fill, outline, weight, and icons — never by hue.

The application is two pages. The **Teller** is a focused, single-column till,
match-first: the operator resolves the customer's wallet-created quote by its
id (typed tail or scan), then settles it — oversized controls, at most two big
buttons per step, and an open-quote list whose ids stay truncated so nothing
can be settled without the customer's code. The **Operator Console** is a
tabbed workspace (Overview, Mint, Keys, Activity, Access) for everything an
administrator does; the Mint tab carries the attachment checklist that
verifies the operator's own cdk-mintd, the Keys tab its keysets' lifetimes.
There is no setup wizard; the stack bootstraps itself and everything is
edited on the running instance.

**Key Characteristics:**
- Strict grayscale: every color token is chroma-0 oklch, light and dark.
- Stock shadcn components (Tailwind v4 variables) — no bespoke token system.
- Meaning through variant, not color: solid = affirm/terminal-good,
  outline = interrupt/in-progress, muted = inactive.
- Two big buttons maximum in the teller flow; no confirmation checkboxes.
- Irreversible or dangerous actions confirm through dialogs where the solid
  button is always the safe path.

## 2. Colors

The palette is the shadcn neutral scale with zero chroma, in both schemes.
Dark mode follows `prefers-color-scheme` automatically.

### Roles
- **Background / Card**: near-white page with white cards (near-black with
  lifted panels in dark).
- **Primary** (`oklch(0.205 0 0)` light, `oklch(0.922 0 0)` dark): the single
  solid fill. Used for the one affirmative action per view, terminal-good
  badges, and the brand mark.
- **Muted / Accent** (`oklch(0.97 0 0)`): tonal backgrounds — table hover,
  segmented controls, chips, tab rails.
- **Destructive**: intentionally mapped to the same values as primary.
  Dangerous actions are `outline` buttons plus explicit dialog copy, never a
  red fill.

### Named Rules

**The Zero Chroma Rule.** No color anywhere, including charts, focus rings,
and toasts. If a value has chroma, it is a bug; `grep` the built CSS for
non-zero oklch chroma to enforce it.

**The Variant Carries Meaning Rule.** Paid/Active = solid badge. Open/
Awaiting-wallet = outline badge. Inactive/Retired/Observed = muted badge.
Failed/Expired = outline badge with an X icon. Health = check/alert icon plus
a text label, never color alone.

**The Solid Is Safe Rule.** In any button pair the solid button is the
expected, affirmative path ("Cash received", "Keep issuing"); the outline
button interrupts or destroys ("Void deposit", "Retire unit").

## 3. Typography

**Fonts:** Inter (self-hosted, variable) for UI; JetBrains Mono for
identifiers, unit codes, amounts, URLs, and quote ids (including the match
input itself).

### Hierarchy
- **Headline** (600, `24px`): page title on each page — one per page.
- **Card Title** (600, `14px`): card headers via shadcn `CardTitle`.
- **Body** (400, `14px`): prose, tables, forms.
- **Supporting** (400–500, `12–13px`, muted-foreground): descriptions, helper
  text, metadata.
- **Mono** (`11.5–12.5px`): everything protocol-shaped.

### The Teller Exception

The teller's settlement steps break the no-hero rule on purpose: the amount
("250 ORA") renders at `36px+`, and the match input is an oversized mono
field, because these are the objects being verified across the counter. On
the matched card the quote id renders with its last six characters
emphasized — the exact characters the operator got from the customer. This
is the only oversized type in the product.

## 4. Elevation

Structural depth only: borders, tonal fills, and shadcn's `shadow-xs` on
cards. Dialogs and toasts use `shadow-lg`. Nothing else casts shadows.

**The No Nested Cards Rule.** Cards contain tables, forms, and detail rows —
never other cards.

## 5. Components

Components are stock shadcn (new-york) with grayscale variants.

### Buttons
- Variants: `default` (solid), `outline`, `secondary`, `ghost`, `link`. The
  old primary/danger/success color variants do not exist.
- Sizes: `sm`, `default`, `lg`, `icon`, and `xl` (48px) — `xl` is reserved for
  the teller's action pair.
- A `loading` prop renders a spinner and disables the control.

### The Action Pair (teller)
The teller's signature component: a full-width row of at most two `xl`
buttons — interrupt as `outline` on the left, proceed as `default` on the
right. States that cannot proceed render only the interrupt, full width.
Never add a third action, a checkbox gate, or checkmark icons.

### Badges
`solid`, `outline`, `muted` — see the Variant Carries Meaning Rule. Small `X`
icons mark failure states.

### Alerts
`default` (bordered card tone) and `emphasis` (foreground-weighted border,
bold title) — emphasis is for operational warnings: do-not-pay-out, demo
credentials active, not-attached-yet. Icons: `TriangleAlert`, `Loader2`.

### Dialogs
`Dialog` for parameterized actions (add user, reset password). `AlertDialog`
for destructive confirmation with the solid-cancel/outline-confirm
convention.

### The Checklist (Mint tab)
The attachment checklist is the console's signature surface, status first.
The Mint tab opens on one card that answers "is it working?": a headline
sentence decided by the first failing check (else warning, else waiting),
a five-segment strip — one per check, with a plain step name, its icon, and a
status word — and the single next step (that check's remedy in a muted
bordered block, or the self-test button when only the test is missing).
Segments follow the Variant Carries Meaning Rule: solid OK, outline
Warning/Failing, muted Waiting. The full rows — icon + title + status badge +
detail + remedy — and the self-test result sit one click away. Icons are
`CircleCheck`, `TriangleAlert`, `CircleAlert`, `CircleDashed`. Remedies are
complete sentences an operator can act on without reading cdk source.

### Disclosures
Secondary and technical detail folds behind a chevron row (native
`<details>`): full checks, the config snippet, protocol facts, advanced form
fields. Rows span the card edge to edge with a top border; a short muted hint
may sit on the right. Detail opens by default only while it is the next thing
the operator needs (the snippet until the mint has attached).

### The Key Timeline (Keys tab)
One lane per keyset on a shared time axis, newest first, with a 1px
foreground "Today" rule. Fill carries the phase, never hue: solid `primary`
while the keyset issues new ecash, `chart-3` hatching while its ecash is
redeem-only, muted fill once expired, a dashed outline before this console
first saw it (start unknown) or for the "if rotated today" preview. A bar ends
at the final expiry; an arrow at the edge means no expiry. Every lane also
carries a status badge (Active / Redeem-only / Expired) and a date in words,
and a tooltip with exact dates. The console only reads keys; rotation is
handed over as an exact `cdk-mint-cli` command.

### Tabs
The console's five sections (Overview | Mint | Keys | Activity | Access) in a
`muted` rail;
the active trigger lifts to `background`. Tabs sync to the URL hash so
`/#mint` deep-links survive refresh. A trigger may carry a small dot for
needs-attention states (failing checklist, demo password).

### Tables
Shadcn tables inside cards (`px-0` content, first/last cells padded `24px`).
Headers are normal-case `14px` medium muted text. Identifiers use mono chips.
Wide tables scroll horizontally inside the card.

### Long lists
Nothing grows without bound. The open-quote list shows ten rows, then "Show
all N"; settled history pages by 25, newest first, with Newer / Older and a
CSV export of the current filters. Filters are segmented controls in the tab
rail's look (muted track, selected option lifted). Relative times ("3h ago")
show the exact date and time on hover.

### Inputs
Stock shadcn inputs. **No read-only inputs**: facts render as text detail
rows; only editable values get form controls. The teller's match input is a
plain form submit on purpose: keyboard-wedge scanners type the payload and
press Enter, so scanning and typing share one code path.

### Toasts
Sonner, monochrome (`popover` surface). Action feedback ("Deposit settled —
500 ORA", "Configuration applied") is a toast; persistent conditions use
inline alerts instead.

### Live mutations
Every config change applies live — nothing restarts. A toast confirms the
save; the SSE stream refreshes open consoles in place. When a change only
takes effect after the operator's own mintd restarts (it reads the
processor's settings at boot), the UI says so in words instead of pretending.

## 6. Do's and Don'ts

### Do:
- **Do** keep the teller to one decision per screen with at most two big
  buttons.
- **Do** use shadcn variables and components before inventing new ones.
- **Do** pair every state with a text label; icons and fills support, never
  replace, words.
- **Do** explain irreversible decisions inside the confirming dialog.
- **Do** keep open-quote ids truncated to their leading characters — the
  matching tail must only ever arrive from the customer.

### Don't:
- **Don't** introduce any color, including for danger, success, or charts.
- **Don't** add confirmation checkboxes or checkmark icons to settlement
  actions.
- **Don't** render read-only values as form inputs.
- **Don't** create marketing surfaces, gradients, glows, or hero sections.
- **Don't** nest cards or add a third button to a teller action row.
- **Don't** assume the operator knows Cashu/CDK internals; prefer plain
  labels ("Deposit", "Withdraw", "Cash received") over protocol terms.
