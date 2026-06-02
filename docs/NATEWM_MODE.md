# NATEWM_MODE — vanilla-dwl `-s` + someblocks intake (design + status)

> **Step 1 — two-channel scaffold:** ✅ landed (commit `ece3ab9`). Skeleton types,
> signatures, lifecycle plumbing, CLI flags.
>
> **Step 2 — parser + listener bodies:** ✅ landed. `DwlStdinBackend::{connect, dispatch}`
> + per-monitor `StatusReducer` (coalesce-latest by overwrite). `SomeblocksIntake::{bind,
> dispatch, cleanup}` with the full policy package (XDG-scoped path, connect-probe
> liveness, non-socket refusal, `Drop` + explicit cleanup). 16 tests pass, including the
> live-fixture parser tests against `tests/fixtures/dwl_stdin_v0.8.txt`. `WmConnection`
> grew an additive `closed(&self) -> bool` default-`false` method for the step-3 host
> EOF-exit wiring.
>
> **Step 3a — two-clocks wiring + UTF-8 robustness:** ✅ landed
> (commits `06c991d` + this commit). Both channels get their own
> `event_loop::add_fd_watch_local` source at `G_PRIORITY_DEFAULT`, with
> `// === Clock 1: WM fd. ===` and `// === Clock 2: someblocks feed fd. ===`
> markers in `app.rs::present_bar`. Neither is tied to the GTK render tick. The
> reducers' coalesce-latest means a slow renderer cannot back-pressure either
> producer. UTF-8 fix-now: titles + block text lossy-decode to mojibake (U+FFFD)
> rather than dropping the line. Core gained `Event::Feed(FeedEvent)` +
> `Topic::Feed` (additive — existing match arms with wildcards unaffected).
>
> **Step 3b — frame rendering + signals + preset + live verify:** ⏳ deferred.
> Build the `feedblocks` plugin (subscribes to `Topic::Feed`, renders the latest
> frame's blocks as a horizontal row of CSS-themable labels); wire SIGTERM/SIGINT
> to `SomeblocksIntake::cleanup()`; observe `WmConnection::closed()` after each
> dispatch and exit (running cleanup) when the compositor goes away; ship
> `themes/natewm-preset.toml`; capture a multi-monitor golden fixture from cage
> with `WLR_WL_OUTPUTS=2` (or real heads); live-verify on dwl via
> `dwl -s 'wafflebar --dwl-status-stdin'` with a `natewm-status` feeder.

## Five flags signed off (step-1 review)

1. `BackendSelect` shape — additive struct (composes, doesn't exclude). ✅
2. `execute()` drop policy on `DwlStdinBackend` — first-drop-per-session info log with
   cause-naming message: *"WM command dropped: dwl `-s` is a one-way status channel,
   clicks/scrolls can't route back without the IPC patch"*. ✅
3. Grammar — docstring + live-captured golden fixture
   (`tests/fixtures/dwl_stdin_v0.8.txt`, raw bytes from `cage` + pinned dwl). ✅
4. Someblocks socket policy — XDG-scoped, hard error on missing XDG (caller-side path
   policy in main.rs leaves `cfg.path = None` → bind returns `Ok(None)` → feed
   disabled), connect-probe liveness, refuse-clobber on non-socket, idempotent cleanup,
   `Drop` + explicit `cleanup()`. ✅
5. Reconnect/EOF — `DwlStdinBackend::closed()` flips on EOF/read-error; host (step 3)
   observes and exits, running the same `SomeblocksIntake::cleanup()` the SIGTERM path
   does. ✅

**Flag 6 (backpressure / coalesce-latest):** built into both reducers by construction.
The `StatusReducer` overwrites per-monitor field values on every applicable line; the
`SomeblocksIntake` per-conn loop overwrites a single `Option<FeedEvent>` candidate. A
100-frame burst from either side produces ≤1 snapshot per dispatch — "never blocks dwl"
is true by construction, not by timing luck.

## Contract (from natewm-asm Phase 4 build prompt — verbatim shape)

- **Vanilla-dwl compatible.** Consumes dwl's `-s` stdin status stream (tags / layout /
  focused title). Runs on unpatched dwl. No IPC patch required to start. Reads or
  closes stdin so it never blocks dwl.
- **Hurmit TTF at configurable size.** Verify it *binds* (fc-match + glyph-shape
  eyeball — same discipline as the dwl titlebar). Don't trust a bare family string.
- **Per-segment fg/bg.** Full Nord palette expressible per module (active tag Frost
  `#88C0D0`, inactive Nord3, etc.).
- **Renders tags + layout symbol + focused title**, matching the natewm mockup.
- **someblocks-style stdin intake** for the right-side clock/mem blocks.
- **Optional IPC** gated behind the dwl IPC patch — never *required* to run.
- **No-bloat budget.** Dep list + binary size recorded in the README.

## The trap, restated

`dwl -s '<cmd>'` connects dwl's status pipe to `<cmd>`'s stdin. That stdin carries
*one* well-defined protocol: dwl status records. **The someblocks-style clock/mem feed
is a separate input** — different schema, different lifecycle (dwl is the producer for
channel 1; an external script is the producer for channel 2). The dwlb near-miss was
trying to multiplex both onto one stdin and losing both sides. This scaffold puts
them in distinct modules from the start so collapsing them is structurally impossible:

```
                  ┌──────────────────────────────┐
   dwl   ── ── ── │ stdin (fd 0)                 │
   (-s)            │ → wm::dwl_stdin              │ ──► WmEvent ──►
                  │   parse status records       │     existing reducer →
                  │   → snapshot()/dispatch()    │     existing tags/layout/
                  └──────────────────────────────┘     focused-title plugins

                  ┌──────────────────────────────┐
   natewm-status  │ UNIX sock @ $XDG_RUNTIME_DIR │
   (separate      │ → feeds::someblocks          │ ──► FeedEvent::Frame ──►
    process)      │   parse block lines          │     right-side strip
                  │   → fd()/dispatch()          │     plugins (step 3)
                  └──────────────────────────────┘
```

Two fds. Two event sources. Two reducers. The host wires both into the GLib loop the
same way (`fd` to watch + `dispatch` on wake-up) — that's the only shared shape.

## Files this scaffold adds

- `crates/wafflebar-core/src/feed.rs` — GTK-free [`FeedBlock`] / [`FeedEvent`] /
  [`FeedSocketConfig`]. Re-exported from `lib.rs`.
- `crates/wafflebar/src/wm/dwl_stdin.rs` — `DwlStdinBackend` skeleton: `WmConnection`
  + `WindowManager` impls, per-monitor reducer struct, protocol grammar in the
  module-level docstring. `connect()` errors today (`scaffold only`).
- `crates/wafflebar/src/feeds/mod.rs` + `feeds/someblocks.rs` — `SomeblocksIntake`
  skeleton: `bind`/`fd`/`dispatch` signatures, socket path policy, color-escapes flag.
  `bind()` errors today (`scaffold only`).

## Files this scaffold *touches*

- `crates/wafflebar-core/src/lib.rs` — `pub mod feed;` + re-exports.
- `crates/wafflebar/src/wm/mod.rs` — `pub mod dwl_stdin;`, new `BackendSelect` struct
  (`force_dwl_stdin: bool`), `connect_backend(BackendSelect)` (was nullary).
- `crates/wafflebar/src/app.rs` — `build_bars` takes `BackendSelect` and threads it
  through. No render-path changes.
- `crates/wafflebar/src/main.rs` — `mod feeds;`, two new CLI flags:
  `--dwl-status-stdin` (force stdin backend) and `--status-socket PATH` (override
  feed socket; default deferred until step 2 binds it).

## What this scaffold deliberately does *not* do

- **No protocol parsing.** The dwl status grammar is in `dwl_stdin.rs`'s module
  docstring but not in code. Step 2.
- **No socket I/O.** `SomeblocksIntake::bind` and `dispatch` are stubs. Step 2.
- **No plugin re-wiring.** Existing tags/layout/title plugins consume `WmEvent` the
  same way; the stdin backend will emit those identically. Step 3 routes
  `FeedEvent::Frame` into right-side strip rendering.
- **No new dependencies.** Stays inside the existing tree.
- **No config-schema changes.** A natewm preset TOML is sketched below but not
  shipped as a file yet — step 3 lands `themes/natewm.toml` once parser + intake
  work.
- **No live-test claim.** All five Phase-3 gates were closed pixel-exact on the
  titlebar; this is Phase 4 Step 1 — structure only. Live test is step 3.

## Reference natewm preset (target — not shipped yet)

```toml
# ~/.config/wafflebar/config.toml — generated by step 3, embedded here as the target

[bar]
position = "top"
height = 26
theme = "nord"            # already-shipped themes/nord.css
layout = "pack"           # left/center/right via CenterBox (P0 work)

# Channel 1 (stdin) feeds tags, layout, focused-title. Channel 2 (socket) feeds clock+mem.
# Both inputs are read-only; no commands flow back to vanilla dwl.

[[bar.modules]]
plugin = "tags"
align = "start"

[[bar.modules]]
plugin = "layout"         # the dwl layout symbol — exists, reads from WmEvent::Layout
align = "start"

[[bar.modules]]
plugin = "window"         # focused title — exists, reads from WmEvent::ActiveWindow
align = "center"
max_chars = 60

# Right-side strip — populated by feeds::someblocks (step 3 plugin route).
# Producer side: natewm-asm/bin/natewm-status writes to the socket.
```

Invocation (step 3 target):

```sh
exec dwl -s 'wafflebar --dwl-status-stdin --status-socket $XDG_RUNTIME_DIR/wafflebar/feed.sock'
# in parallel (sidecar):
natewm-status | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/wafflebar/feed.sock
```

## Hurmit binding — fc-match closes the silent-failure trap

Verified for this scaffold (`themes/nord.css` already requests `Hurmit Nerd Font Mono`):

```
$ fc-match -f '%{family}|%{file}\n' 'Hurmit Nerd Font Mono'
Hurmit Nerd Font Mono|/usr/share/fonts/hurmit/HurmitNerdFontMono-Regular.otf
```

No fallback. Same discipline that paid off on the dwl titlebar (commit `e3c582c` →
verification round). Re-run as part of every step's live-verify; don't trust the
family string in the CSS.

## Definition of done (Phase 4 overall)

> `dwl -s 'wafflebar --dwl-status-stdin'` on **unpatched** dwl looks indistinguishable
> from the existing dwlb setup, with the `natewm-status` feed untouched. The
> someblocks feed populates the right strip via its own socket. The dwl IPC patch is
> *optional* — without it, the stdin backend wins and the bar still renders tags,
> layout, focused title, plus the feed blocks.

Scope stays to the contract. No feature creep; the GTK4 panel substrate keeps its
plugin set (existing config opts in/out of plugins like today).

## Rebase notes

- `dwl_stdin.rs` is the highest-churn surface — the dwl status grammar is the
  rebase-fragile thing. The grammar lives in *one* module docstring and *one*
  parser function (step 2). Both are grep-friendly (`dwl_stdin_` / `dwl_stdin::`)
  so a dwl-tag bump that changes the protocol is mechanical to track.
- The `BackendSelect` shape is additive (default-constructed = today's behavior).
  Adding fields later won't break the call site in `app.rs::build_bars`.
- `FeedSocketConfig` lives in core (GTK-free) so the eventual external-process
  isolation step can transport it across a boundary without a rewrite — same
  discipline as `WmEvent`.
