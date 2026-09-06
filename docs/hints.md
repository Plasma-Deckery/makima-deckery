# Hints — display-only labels for shortcuts that already work

**Status: implemented.** Section name is `[hints]`. Everywhere the shipped code
differs from the design as first written, the section is marked *(changed
during implementation)*.

## The problem

Some of the most useful shortcuts on the Deck are never *bound* — they simply
fall out of the base config. `R5` remaps to `KEY_LEFTCTRL`, `Up` remaps to
`KEY_UP`, so holding R5 and pressing Up sends Ctrl+Up to the focused app. That
works today, with no entry anywhere.

Because there is no entry, the HUD cannot show it. Every such shortcut is
invisible: the user has to already know it exists.

Hints close that gap. A hint attaches a label to a button *combination* without
creating a binding, without touching the event path, and without changing what
any key does.

## Why the obvious approaches fail

### Promoting the paddle to a custom modifier

The intuitive fix is to make `R5` a custom modifier and write `R5-Up = { keys =
[...], label = "Jump to Top" }`. Two things break.

**`L1-R5` dies.** A button that is both a custom modifier *and* carries a base
remap hits the Custom Modifier Intercept at
[`event_reader.rs:1329`](../src/event_reader.rs). That branch emits the remap
output, tracks the input key, and then `return`s at
[`event_reader.rs:1372`](../src/event_reader.rs) — before `resolve_binding` ever
runs. Any combo with R5 on the *right* side is unreachable from that moment on.
In the live config that is the Voice Control module's `L1-R5` (OpenWhispr).

**The automatic modifier pass-through is lost.** Writing `R5-Up` explicitly makes
it a combo, which sets `ignore_modifiers = true`, whose branch actively releases
the modifier's *output* key. Ctrl would be dropped before `KEY_UP` is emitted, so
the full output (`KEY_LEFTCTRL` + `KEY_HOME`, etc.) would have to be spelled out
by hand in every entry.

### What must keep working

Unbound modifier combinations survive today through step 5 of the resolver,
[`resolver.rs:133`](../src/resolver.rs): no combo match → fall back to the base
remap with `is_fallback = true`, which deliberately does **not** release the held
modifier outputs. Ctrl stays down, `KEY_UP` goes out, the OS sees Ctrl+Up.

Hints must not disturb this. The whole design follows from that constraint.

## Design

A new `[hints]` section maps a button combination to a label. It creates no
binding, produces no output, and registers no modifier.

```toml
[hints]
KEY_LEFTCTRL-KEY_UP    = "(Jump to Top)"
KEY_LEFTCTRL-KEY_DOWN  = "(Jump to Bottom)"
KEY_LEFTALT-KEY_LEFT   = "(Back)"
```

Parentheses are a **config convention, not a code feature** — nothing in the
HUD or the parser adds or strips them. They mark a label as "this is only a
description" and stay optional, so a hint like `KEY_ENTER = "Enter (Send
Message)"` can spell itself out however reads best.

### Two forms *(changed during implementation)*

A hint key with no `-` has no modifier side. It resolves to an **empty combo**
and relabels a plain button:

```toml
[hints]
KEY_ENTER = "Enter (Send Message)"    # → the button that emits Enter (A)
KEY_ESC   = "Esc (Abort)"             # → the button that emits Esc  (B)
```

The original design rejected this and warned about it, on the grounds that
`label` on the binding already covers it. That was wrong in practice: using
`label` forces the app config to *re-declare the keys* just to attach a name —
`A = { keys = ["KEY_ENTER"], label = "…" }` — which creates a real binding, flips
`origin`, and duplicates a line of the base config that then has to be kept in
sync by hand. The modifier-less hint says only what it means. It is the
`label_only` idea from the rejected-alternatives table, arriving through the
hints syntax instead of as its own attribute.

Both forms share `hints_resolved`. The empty combo is what routes an entry into
`bindings` instead of `modifier_active`.

### Separator and namespace *(changed during implementation)*

The separator is `-`, the same as every other section. There is no second
syntax.

Every segment is a `KEY_*` name, resolved through the reverse index to the
buttons that emit it. There is **no input-space form**. Button names and device
aliases (`A`, `BTN_SOUTH`, `L1`) are refused with a warning.

The first draft allowed both spaces and called mixing "legal and harmless". It
is neither. A hint describes the shortcut the *focused application* listens for;
the button is what the lookup returns, never what it is given. Naming a button
directly skips the lookup and asserts a mapping instead of describing one — and
for a combination no application can observe. `L1-A` is not a shortcut anybody
can press "at" an app; if you want that, it wants a real binding, not a label.

The cost is accepted: a combination is only hintable when every part of it sends
a key. `L1-…`, `Steam-…` and R5-as-a-button cannot be hinted.

This is why `+` was considered and dropped — see *Rejected alternatives*.

### Resolution happens after the merge

The reverse index is built from base remaps only (`combo == []`) of the **merged**
config. It must not be built while parsing a single file: a module or an app
override can move `KEY_LEFTCTRL` to a different button, and hints live in app
configs.

Resolving once per config activation also keeps it off the hot path —
`state.json` is rewritten on every button event.

### The reverse index returns a set, not a single button

Two buttons may emit the same key. This is not hypothetical; the live base config
has it:

```
Steam Deck.toml:26   R2 = { keys = ["BTN_LEFT"], ... }
Steam Deck.toml:47   R3 = ["BTN_LEFT"]
```

Rule: **a hint that resolves to several buttons is shown on all of them**, plus a
warning at load time. Silently picking one would be the worst outcome. In
practice this will rarely fire — nobody writes a hint on `BTN_LEFT`.

The mirror case is two *lines* reaching one button, which happens when a button
sends several keys (`Y = ["KEY_SPACE", "KEY_X"]`, both hinted). Only one label
fits, so the loser is dropped — with a warning, and deterministically: the raw
keys are resolved in sorted order, so the winner is the same on every run.

### Matching is exact

The comparison reuses the rule already used for real combos, extracted as
`shown_under` at [`state_export.rs:263`](../src/state_export.rs): length equality
*plus* subset. All four passes call it, so hints and real combos cannot drift.

- `KEY_LEFTCTRL-KEY_LEFTSHIFT-KEY_M` fires only when exactly {Ctrl, Shift} are held.
- `KEY_LEFTCTRL-KEY_M` disappears the moment Shift is added.
- Real modifiers participate in the comparison set. Holding `L1` *and* `R5` hides
  R5-only hints, because L1 opens a real layer.

### Precedence *(changed during implementation)*

A real binding always wins over a hint on the same combination. A typo in
`[hints]` must never mask a working binding.

Applied in `resolve_hints` in `src/config.rs`, not in `state_export.rs` as this
document first assumed. Doing it at resolution time means a losing hint never
enters `hints_resolved` at all, so every consumer downstream — `modifier_active`,
`bindings`, `available_modifiers` — inherits the rule for free instead of each
re-implementing it.

Modifier-less hints need the inverted rule: the base remap they resolved
*through* is exactly the binding they are meant to label, so it cannot
disqualify them. Only a `label` written out on that binding outranks them.

## What lands in state.json

**`modifier_active`** — combo hints are inserted as a fourth pass after the
existing remap/command/movement loops, with `"action": []`, `"kind": "hint"` and
the label set, only where none of the three real passes already wrote that
button. Running last *and* checking `contains_key` is what implements "a real
binding always wins" at the export layer.

**`bindings`** *(changed during implementation)* — modifier-less hints go here
instead, since there is no modifier to wait for. They overwrite `label`,
`origin` and `kind` on the existing entry and **leave `action` alone**: the
action stays the HUD's fallback text and keeps `active_outputs` honest. A hint
on a button with no binding at all inserts a fresh entry with `"action": []`.

`kind: "hint"` is informational here — no renderer branches on it. It stays
because `origin` does flip to the app config, and without `kind` the entry would
read as "this app rebound A", which is exactly what hints exist not to do.

**`available_modifiers`** — hint modifiers are added as regular entries carrying
`"virtual": true`. This is what tells the HUD "there is something to discover
here" *before* the button is pressed. It widens the meaning of the field from
"unlocks a combo" to "unlocks a combo **or** reveals hints"; that shift is
recorded in the comment above [`state_export.rs:445`](../src/state_export.rs).

**`held_modifiers` — deliberately NOT touched.** It means makima's own internal
button modifiers. Adding virtual entries there would contradict the model. This
was raised and rejected.

## The HUD *(changed during implementation)*

The first version needed no HUD changes, and that was true: the existing code
tolerates every new field. But "renders without error" turned out not to mean
"reads correctly". Two signals were actively wrong.

**Hints were painted amber.** `callouts.py` sets `amber = active_mod or is_combo`,
and a hint arrives through `modifier_active` like any combo, so it inherited the
"a binding fires here" colour. Combined with the app-specific origin that made it
`conflict`: teal text plus an amber underline. The underline is reserved for
*"an app override that a held modifier unlocked"* — a hint unlocks nothing. Fixed
by excluding `kind == "hint"` from the amber tier, which drops it into the plain
`layer_override` branch: teal, no underline.

The exclusion is read **only from the `modifier_active` entry**, never from the
`bindings` one. A modifier-less hint reaches the amber tier by exactly one route
— its button being a held modifier itself, as `KEY_LEFTCTRL = "Ctrl"` would be
on R5 — and there the amber is correct and must survive. Reading `kind` from whichever
entry happened to win would silently strip it.

**The availability diamond was amber.** Amber means "combos behind this button".
Behind a hint modifier there are only labels. `virtual: true` now paints the
diamond teal — the same colour as the labels it reveals, so the marker and what
it opens read as one thing. The teal `has_app_combos` stroke is skipped in that
case; under a teal fill it carries no information.

Unchanged and still relied on: `callouts.py` renders `b.get("label") or
_fmt(b["action"])` and treats an entry as present when it has *either* an action
or a label, so `"action": []` plus a label works.

**Origin** comes from `Hint::from_override`, set by `resolve_hints`. The merge
flattens base and override hints into one map, so without it every hint would
report the active config and a base-config hint would render as an app-specific
override in every app. Inert with today's configs — all hints live in app
configs — and load-bearing the moment the first hint is written into the base.
The same flag answers `has_app_combos` for a virtual modifier, which asks the
identical question one level up: is anything behind this button app-specific?

## Warnings at load time

Surfaced through `ConfigRegistry::hint_warnings()`, called once from `main.rs`
*(changed during implementation)*. Resolution itself happens on every config
activation in `ConfigRegistry::resolve()`, which is on the hot path — so
`resolve()` discards its warnings and the load-time pass re-runs the merges
purely to collect them. Never fatal: a hint is display-only.

| Condition | Reason |
|---|---|
| Segment is not a `KEY_*` name | Input space is refused. Says so explicitly, because "nothing happened" is the failure mode a button name would otherwise produce. |
| Segment is not a known key | Typo in a `KEY_*` name. |
| Hint resolves to no button | Dead hint — can never be displayed. Only detectable because hints are written in output space. |
| Hint resolves to several buttons | Ambiguous; shown on all, user should know. |
| Two hints resolve to the same button | Only one label fits. The loser is dropped, and named, so it does not look like a resolver bug. |

The originally planned *"hint has no modifier"* warning is gone — that spelling
is now the modifier-less form.

## Limits

**Hints only land on buttons with a plain base remap.** `KEY_C` is emitted only
by the combo `L1-X`, never by a base remap, so `KEY_LEFTCTRL-KEY_LEFTSHIFT-KEY_C`
resolves to nothing and correctly refuses. There is no single button that yields
C while Ctrl+Shift are held. Shortcuts like that need a real binding — the hint
mechanism does not help.

The useful range is exactly: `KEY_LEFTCTRL` / `KEY_LEFTALT` / `KEY_LEFTSHIFT` /
`KEY_LEFTMETA` as the modifier side, and the arrow keys, Enter, Esc, Backspace,
Space, Tab, F10 as the trigger side. That is the full set of plain base remaps in
`Steam Deck.toml`, and it covers what hints are for.

**Hints are unverified assertions.** Nothing checks that Ctrl+Up does anything in
the focused app. A wrong hint is a silent display error — the same risk class as
the existing `keys = []` labels, but at larger scale.

## One wrinkle: `KEY_LEFTCTRL` means two different things

`KEY_LEFTCTRL` is in the hardcoded `default_modifiers` list at
[`config.rs:740`](../src/config.rs), together with Shift, Alt, Meta and the Right
variants. Those go into `mapped_modifiers.all` and are tracked by
`toggle_modifiers` when they arrive as *input* events.

So today, `KEY_LEFTCTRL-BTN_NORTH` in a `[remap]` block already has a meaning:
"a real Ctrl key on a real keyboard is held". That is makima's original keyboard
semantics.

In `[hints]` the same string means the opposite: "a button that *emits*
`KEY_LEFTCTRL` is held".

On the Steam Deck this is inert — the gamepad never emits `KEY_LEFTCTRL` as
input, so the remap meaning is dead on this device. But it is a genuine semantic
shift between sections, and belongs in a comment above every `[hints]` section.
Today that is only `apps/Claude Desktop.toml`; the base config has no hints.

The "emits X" notion is not an invention: [`state_export.rs:226`](../src/state_export.rs)
already treats a custom modifier as active when its output key is among the held
modifiers. Hints build on that precedent.

## Rejected alternatives

| Alternative | Why rejected |
|---|---|
| `+` as an output-space separator (`"Ctrl+Up"`) | TOML bare keys allow only `A-Za-z0-9_-`. Every hint would need quotes, in a file that otherwise has none. It was meant to mark "this line is output space"; once *every* line is, there is nothing left to mark. |
| Allowing input space, with a "one key, one namespace" validation rule | Both dropped together *(changed during implementation)*. Mixing is well-defined mechanically, which is why the first draft allowed it — but a hint on a button names a combination no application can observe. Refusing input space outright removes the need for the rule. |
| Hints keyed on physical buttons only (`R5-Up`) | Breaks silently when the modifier is moved to another button. Output space follows the remapping and makes dead hints detectable. |
| Promoting `R5` to a custom modifier | Kills `L1-R5` via the intercept `return`; forces every output to be spelled out. |
| Adding virtual modifiers to `held_modifiers` | Contradicts the meaning of that field. |
| A `label_only` attribute on individual bindings | Considered earlier in the same discussion. Per-key, so it cannot express "when Ctrl is held". Superseded by this design — and then *absorbed* by it: the modifier-less hint is that feature, reached through the same syntax rather than a second one. |

## What was built

| File | Work |
|---|---|
| `src/config.rs` | `[hints]` in `RawConfig`; verbatim storage in `bindings.hints` (never through `parse_binding_input`, which would register modifiers); merge rule identical to `labels`; `resolve_hints()` with the reverse index, output-space-only resolution, set-valued results, collision detection, and precedence |
| `src/config_registry.rs` | `resolve()` resolves hints post-merge; `hint_warnings()` collects them once at load |
| `src/main.rs` | Prints the warnings, non-fatal |
| `src/state_export.rs` | Hint comparison set from `held_keys`; modifier-less pass into `bindings`; combo pass into `modifier_active`; virtual entries in `available_modifiers` |
| `deckery-hud/src/callouts.py` | `kind == "hint"` excluded from the amber tier; teal diamond for `virtual: true` |
| tests | 28 — resolution, both forms, exact matching, ambiguity in both directions, dead hints, input space refused, collision determinism, precedence in both directions, virtual-modifier origin, no leak into `mapped_modifiers` or `held_modifiers` |

Event path: untouched, as designed. HUD: two colour rules, which the original
plan predicted would be zero.

## Invariants a reviewer should check

1. Nothing from `[hints]` ever enters `mapped_modifiers` (`.custom`, `.default`
   or `.all`).
2. `resolve_binding` is never called with hint data and never returns it.
3. `L1-R5` still resolves after hints are loaded.
4. A real binding on a button suppresses a hint on the same combination —
   *except* for the modifier-less form, where the base remap is the thing being
   labelled and only a written-out `label` outranks it.
5. `held_modifiers` in state.json is unchanged.
6. A modifier-less hint never reaches `modifier_active` and never turns its own
   trigger into an entry in `available_modifiers`.

## Deferred: `last_action`

A hint currently produces no OSD toast. Pressing R5+Up fires the base remap, so
`set_last_emitted` ([`event_reader.rs:2012`](../src/event_reader.rs)) records
nothing useful — and note that R5 is not a modifier at all, so neither
`is_combo` nor `is_fallback` is set on that path. The earlier draft of this
document had that wrong.

The hook would go in `set_last_emitted`: on `value == 1`, compare the held
buttons against `hints_resolved` with the same length-plus-subset rule, and on
an exact match write the hint's label as `LastAction`. Read-only,
`resolve_binding` untouched, emitted keys unchanged — invariant 2 survives.

Deferred deliberately, to let the display side settle first. The display side has
since landed, so this is now tracked as
[deckery-hud#20](https://github.com/Plasma-Deckery/deckery-hud/issues/20).

## Related open items from the same discussion

Not part of this design, but decided or deferred alongside it and easy to lose:

- **Dropped:** `Ctrl+Shift+C` (copy last response) in `apps/Claude Desktop.toml`.
  It cannot be a hint — no button emits `KEY_C` — so it would have needed a real
  binding, and the shortcut itself came from a web source that was already wrong
  twice. Both candidate placements were poor: `···` is `BTN_BASE`, which is also
  the Gaming Mode double-click trigger (`Steam Deck.toml:89`), so a double tap
  would toggle Gaming Mode. Not worth a binding.
- **Closed:** `Ctrl+A`, zoom reset (`Ctrl+0`), and copy/paste are not needed —
  the latter is already covered by `L1-X` / `L1-Y` in the base config.
- **Untested:** the startup-focus fix (`event_reader.rs:408` one-shot
  `update_config()` before the notify loop, plus the load-time push in the KWin
  script) has no regression test.
- **Closed:** the focus-notification race in `window_changed_loop`
  ([`event_reader.rs:406`](../src/event_reader.rs)). The waiter used to be
  registered only at the *top* of each iteration, so nothing was listening while
  `update_config()` ran — and `notify_focus_change`
  ([`compositor/mod.rs:52`](../src/compositor/mod.rs)) uses `notify_waiters()`,
  which stores no permit, so a push into that gap was lost outright. The gap
  spanned `registry.resolve()` (full merge plus `resolve_hints`) and
  `write_state()`, i.e. **milliseconds, recurring on every iteration** — not
  just at startup; two rapid window switches could drop the second.
  Fixed with `Notified::enable()`: the future is pinned and enabled *before*
  `active_client` is read, and re-armed *before* the work rather than after.
  (An earlier revision of this document, and the commit message of `3ee9a9f`,
  called this a microsecond race confined to startup. That was wrong.)
