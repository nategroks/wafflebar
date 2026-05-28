//! libpulse backend for the volume plugin. **All** libpulse usage lives here; the reducer
//! (`super`) only ever sees typed [`VolumeEvent`]s and issues typed [`VolumeCommand`]s.
//!
//! It plugs into the *existing* GLib main loop via `libpulse-glib-binding` (a `pa_glib_mainloop`
//! attached to the default `GMainContext`), so subscription events wake our loop — no extra thread,
//! no polling. Lifecycle mirrors xfce4-pulseaudio-plugin's `pulseaudio-volume.c`: context init →
//! connect → on Ready subscribe to sink/server events → on any event re-query the default sink →
//! derive volume/mute → emit. v1 tracks the **default sink only**.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::SinkInfo;
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::volume::Volume;
use libpulse_glib_binding::Mainloop;
use tracing::warn;
use wafflebar_core::{VolumeCommand, VolumeEvent, VOLUME_NORM};

/// Sink set by the volume plugin; the backend invokes it on every derived audio change.
type Handler = Rc<RefCell<Option<Box<dyn Fn(VolumeEvent)>>>>;

/// How long to wait before rebuilding the libpulse connection after it drops (e.g. a PipeWire /
/// pipewire-pulse restart). The retry loop is self-sustaining (see `configure_and_connect`).
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

pub struct PulseBackend {
    context: Rc<RefCell<Context>>,
    // Kept alive: it owns the GSource attached to our GMainContext that drives libpulse callbacks.
    // `Rc` so the reconnect path can build a fresh context on the same mainloop.
    _mainloop: Rc<Mainloop>,
    handler: Handler,
}

impl PulseBackend {
    /// Set up libpulse on the GLib default main context and begin connecting. Returns `None` only
    /// if libpulse can't be initialised at all; connection itself is async and a failure surfaces
    /// later as [`VolumeEvent::Unavailable`] through the handler (so the plugin renders nothing) —
    /// and then auto-reconnects (see `configure_and_connect`).
    pub fn new() -> Option<Self> {
        let mainloop = Rc::new(Mainloop::new(None)?); // None → the default GMainContext, i.e. our loop
        let context = Rc::new(RefCell::new(Context::new(&*mainloop, "wafflebar")?));
        let handler: Handler = Rc::new(RefCell::new(None));
        configure_and_connect(&mainloop, &context, &handler)?;
        Some(Self { context, _mainloop: mainloop, handler })
    }

    /// Install the event handler. Called once after the host exists; should hold a `Weak` to the
    /// host to avoid a reference cycle (host → volume_sink → backend → handler → host).
    pub fn set_handler(&self, handler: impl Fn(VolumeEvent) + 'static) {
        *self.handler.borrow_mut() = Some(Box::new(handler));
    }

    /// Perform a [`VolumeCommand`] on the default sink. Re-queries current state first (volumes are
    /// relative). If the context isn't `Ready` (e.g. mid-reconnect), the introspect ops simply
    /// don't run — no panic; the next subscription event re-syncs the displayed value.
    ///
    /// Rapid-fire scrolls settle correctly: libpulse processes a context's operations in
    /// submission order, so each scroll's `get_sink_info` sees the prior scroll's `set_sink_volume`
    /// already applied — the read-modify-writes compound rather than racing.
    /// TODO(volume): integration test against a fake libpulse to assert final-value settling.
    pub fn execute(&self, cmd: &VolumeCommand) {
        if self.context.borrow().get_state() != State::Ready {
            return;
        }
        let ctx = self.context.clone();
        let cmd = *cmd;
        // Resolve the default sink, then apply against its current state.
        let introspect = self.context.borrow().introspect();
        introspect.get_server_info(move |server| {
            let Some(name) = server.default_sink_name.as_ref().map(|n| n.to_string()) else {
                return;
            };
            let ctx = ctx.clone();
            let introspect = ctx.borrow().introspect();
            introspect.get_sink_info_by_name(&name, move |res| {
                if let ListResult::Item(info) = res {
                    apply(&ctx, info, cmd);
                }
            });
        });
    }
}

/// Install the state callback on `slot`'s context and begin connecting. On `Ready` → subscribe +
/// prime; on `Failed`/`Terminated` → emit `Unavailable` and, after [`RECONNECT_DELAY`], build a
/// *fresh* context (libpulse contexts aren't reusable once terminated) and reconnect — so volume
/// self-heals across a PipeWire restart. The retry is self-sustaining (each failed attempt's context
/// re-enters this on its own `Failed`). Weak refs to the slot + mainloop avoid a reference cycle with
/// the context that owns the callback, and stop reconnecting once the backend is dropped.
fn configure_and_connect(
    mainloop: &Rc<Mainloop>,
    slot: &Rc<RefCell<Context>>,
    handler: &Handler,
) -> Option<()> {
    let (ml_weak, slot_weak, handler_cb) =
        (Rc::downgrade(mainloop), Rc::downgrade(slot), handler.clone());
    slot.borrow_mut().set_state_callback(Some(Box::new(move || {
        // libpulse calls this synchronously inside Context methods while we hold `borrow_mut`; defer
        // the real work to a GLib idle tick so the borrow is released and we're on a clean stack.
        let (ml_weak, slot_weak, handler) = (ml_weak.clone(), slot_weak.clone(), handler_cb.clone());
        glib::idle_add_local_once(move || {
            let Some(slot) = slot_weak.upgrade() else { return };
            let state = slot.borrow().get_state(); // drop the borrow before the arms re-borrow
            match state {
                State::Ready => subscribe_and_prime(&slot, &handler),
                State::Failed | State::Terminated => {
                    emit(&handler, VolumeEvent::Unavailable);
                    let (ml_weak, slot_weak, handler) =
                        (ml_weak.clone(), slot_weak.clone(), handler.clone());
                    glib::timeout_add_local_once(RECONNECT_DELAY, move || {
                        let (Some(ml), Some(slot)) = (ml_weak.upgrade(), slot_weak.upgrade()) else {
                            return; // backend dropped — stop reconnecting
                        };
                        if let Some(ctx) = Context::new(&*ml, "wafflebar") {
                            *slot.borrow_mut() = ctx; // drop the dead context, install a fresh one
                            let _ = configure_and_connect(&ml, &slot, &handler);
                        }
                    });
                }
                _ => {}
            }
        });
    })));
    if slot.borrow_mut().connect(None, FlagSet::NOFLAGS, None).is_err() {
        warn!("volume: libpulse Context::connect failed; no audio backend");
        return None;
    }
    Some(())
}

/// Subscribe to sink/server changes and prime the current value once.
fn subscribe_and_prime(context: &Rc<RefCell<Context>>, handler: &Handler) {
    {
        // No completion callback needed.
        let mut ctx = context.borrow_mut();
        ctx.subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});
    }
    {
        let ctx_weak = Rc::downgrade(context);
        let handler_sub = handler.clone();
        context
            .borrow_mut()
            .set_subscribe_callback(Some(Box::new(move |_facility, _op, _idx| {
                if let Some(ctx) = ctx_weak.upgrade() {
                    query_default_sink(&ctx, &handler_sub);
                }
            })));
    }
    query_default_sink(context, handler); // initial value
}

/// Read the default sink's volume/mute and emit a typed event.
fn query_default_sink(context: &Rc<RefCell<Context>>, handler: &Handler) {
    let ctx = context.clone();
    let handler = handler.clone();
    let introspect = context.borrow().introspect();
    introspect.get_server_info(move |server| {
        let Some(name) = server.default_sink_name.as_ref().map(|n| n.to_string()) else {
            emit(&handler, VolumeEvent::Unavailable);
            return;
        };
        let handler = handler.clone();
        let introspect = ctx.borrow().introspect();
        introspect.get_sink_info_by_name(&name, move |res| {
            if let ListResult::Item(info) = res {
                emit(
                    &handler,
                    VolumeEvent::SinkVolumeChanged {
                        sink_index: info.index,
                        volume: info.volume.avg().0,
                        muted: info.mute,
                    },
                );
            }
        });
    });
}

/// Apply a command to a known sink's current state.
fn apply(context: &Rc<RefCell<Context>>, info: &SinkInfo, cmd: VolumeCommand) {
    let mut introspect = context.borrow().introspect();
    match cmd {
        VolumeCommand::ToggleMute => {
            introspect.set_sink_mute_by_index(info.index, !info.mute, None);
        }
        VolumeCommand::Adjust { delta } => {
            let cur = info.volume.avg().0 as i64;
            let step = (VOLUME_NORM as i64) * delta as i64 / 100;
            let target = (cur + step).clamp(0, VOLUME_NORM as i64) as u32;
            let mut volume = info.volume;
            volume.set(volume.len(), Volume(target));
            introspect.set_sink_volume_by_index(info.index, &volume, None);
        }
        VolumeCommand::SetVolume { percent } => {
            let target = ((VOLUME_NORM as u64) * percent.min(100) as u64 / 100) as u32;
            let mut volume = info.volume;
            volume.set(volume.len(), Volume(target));
            introspect.set_sink_volume_by_index(info.index, &volume, None);
        }
    }
}

fn emit(handler: &Handler, event: VolumeEvent) {
    if let Some(f) = handler.borrow().as_ref() {
        f(event);
    }
}
