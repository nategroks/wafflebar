//! Wakeup source for the compositor backend's `wl_display` fd.
//!
//! The dispatch contract is unchanged from M2 (`prepare_read` → poll → `read_events` →
//! `dispatch_pending`, flush after every command). This module only changes *how* we learn the fd
//! is readable: an fd-driven GLib source instead of a 20 Hz poll, so a truly idle bar makes zero
//! syscalls instead of 20/second.
//!
//! glib-rs 0.22.7 has no safe `unix_fd_add_local` binding (verified against the pinned source — it
//! has `idle`/`timeout`/`child_watch` sources but no fd source), and glib-sys 0.22.6 doesn't export
//! `g_unix_fd_add` either, so we declare the C entry point ourselves. `g_unix_fd_add_full` lives in
//! `glib-unix.h` / `libglib-2.0`, linked transitively via glib-sys. All unsafe is contained here;
//! the public [`add_fd_watch_local`] is a safe wrapper with the same `ControlFlow` callback shape as
//! the `glib::timeout_add_local` it replaces.

use std::os::unix::io::RawFd;

use gtk4::glib;
use glib::ffi as gffi;
use glib::translate::FromGlib;
use glib::{ControlFlow, SourceId};

// GUnixFDSourceFunc: gboolean (*)(gint fd, GIOCondition, gpointer)
type GUnixFdSourceFunc =
    unsafe extern "C" fn(RawFd, gffi::GIOCondition, gffi::gpointer) -> gffi::gboolean;

extern "C" {
    fn g_unix_fd_add_full(
        priority: i32,
        fd: i32,
        condition: gffi::GIOCondition,
        function: GUnixFdSourceFunc,
        user_data: gffi::gpointer,
        notify: gffi::GDestroyNotify,
    ) -> u32;
}

/// Invoke `callback` on the thread-default main context whenever `fd` becomes readable (`G_IO_IN`).
/// The callback returns [`ControlFlow`] exactly like the timer source it replaces — `Break`
/// unregisters the source. Returns the [`SourceId`] so the caller can remove it on clean shutdown.
///
/// `_local` (non-`Send` closure) is sound here because the GLib main loop is single-threaded and we
/// only ever register on it from the main thread.
pub fn add_fd_watch_local<F>(fd: RawFd, callback: F) -> SourceId
where
    F: FnMut() -> ControlFlow + 'static,
{
    unsafe extern "C" fn trampoline<F: FnMut() -> ControlFlow + 'static>(
        _fd: RawFd,
        _condition: gffi::GIOCondition,
        data: gffi::gpointer,
    ) -> gffi::gboolean {
        // The boxed closure outlives every call until `destroy` runs (on source removal).
        let callback = &mut *(data as *mut F);
        match callback() {
            ControlFlow::Continue => gffi::GTRUE,
            ControlFlow::Break => gffi::GFALSE,
        }
    }

    unsafe extern "C" fn destroy<F>(data: gffi::gpointer) {
        drop(Box::from_raw(data as *mut F));
    }

    let boxed = Box::into_raw(Box::new(callback)) as gffi::gpointer;
    // SAFETY: `g_unix_fd_add_full` keeps `boxed` alive until it calls `destroy::<F>`, which reclaims
    // the Box. `trampoline::<F>` only ever sees that pointer. `fd` must stay open for the source's
    // lifetime — the caller guarantees this by keeping the backend (which owns the fd) alive as long
    // as the returned SourceId is registered.
    let id = unsafe {
        g_unix_fd_add_full(
            gffi::G_PRIORITY_DEFAULT,
            fd,
            gffi::G_IO_IN,
            trampoline::<F>,
            boxed,
            Some(destroy::<F>),
        )
    };
    // g_unix_fd_add_full returns a non-zero source id on success.
    unsafe { SourceId::from_glib(id) }
}
