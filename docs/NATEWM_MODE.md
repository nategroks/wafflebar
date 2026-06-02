# NATEWM_MODE — vanilla-dwl `-s` + someblocks intake (design + status)

> **Step 1 — two-channel scaffold:** 🚧 in progress (this commit). Skeleton types,
> signatures, lifecycle plumbing, CLI flags, no parser bodies. Compiles clean (two
> `dead_code` warnings on the as-yet-unused `SomeblocksIntake` are suppressed —
> step 2 wires them up).
>
> **Step 2 — parser + listener bodies:** ⏳ deferred. Fill in `DwlStdinBackend::connect`,
> `dispatch`, the per-monitor reducer, and `SomeblocksIntake::bind`/`dispatch`/loop.
>
> **Step 3 — host integration:** ⏳ deferred. Plug `SomeblocksIntake.fd()` into the GLib
> main loop next to the existing WM fd; route `FeedEvent::Frame` to the right-side strip
> plugins. Live-verify on dwl via `dwl -s 'wafflebar --dwl-status-stdin'` with a
> `natewm-status` feeder writing to the socket.

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
