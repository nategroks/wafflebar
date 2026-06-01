//! statvfs() polling backend for the disk plugin. Discovers mount points from `/proc/mounts`
//! (filtering to real filesystems — skips tmpfs, proc, sysfs, etc.), polls each every 30 s. The
//! plugin filters the emitted events by its configured `path`.
//!
//! Same shape as memory: `glib::timeout_add_seconds_local`, no thread.

use std::collections::HashSet;
use std::ffi::CString;
use std::os::raw::c_char;
use std::time::Duration;

use gtk4::glib;
use tracing::warn;
use wafflebar_core::DiskState;

const POLL_SECS: u64 = 30;

/// Filesystem types that contain "real" data the user cares about. Everything else (tmpfs, overlay,
/// proc, sysfs, cgroups, autofs, debugfs, etc.) is skipped.
const REAL_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "zfs", "reiserfs", "jfs",
    "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "nfs", "nfs4", "cifs", "smb3",
];

pub trait DiskBackend {
    fn start(self, emit: Box<dyn Fn(String, DiskState)>) -> glib::SourceId;
}

pub struct StatvfsBackend;

impl DiskBackend for StatvfsBackend {
    fn start(self, emit: Box<dyn Fn(String, DiskState)>) -> glib::SourceId {
        // Tick once now so plugins aren't blank until the first interval elapses.
        for p in mount_points() {
            if let Some(s) = statvfs(&p) {
                emit(p, s);
            }
        }
        glib::timeout_add_seconds_local(POLL_SECS as u32, move || {
            for p in mount_points() {
                if let Some(s) = statvfs(&p) {
                    emit(p, s);
                }
            }
            glib::ControlFlow::Continue
        })
    }
}

/// Walk /proc/mounts, return unique mount-point paths for real filesystems.
fn mount_points() -> Vec<String> {
    let raw = match std::fs::read_to_string("/proc/mounts") {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "disk: cannot read /proc/mounts");
            return Vec::new();
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for line in raw.lines() {
        // Format: <device> <mountpoint> <fstype> <options> <dump> <pass>
        let mut it = line.split_whitespace();
        let _dev = it.next();
        let mp = match it.next() { Some(s) => s, None => continue };
        let fs = match it.next() { Some(s) => s, None => continue };
        if !REAL_FS.contains(&fs) { continue; }
        // /proc/mounts escapes spaces as \\040; decode the common case.
        let mp_dec = mp.replace("\\040", " ");
        if seen.insert(mp_dec.clone()) {
            out.push(mp_dec);
        }
    }
    out
}

/// One statvfs() call. Returns None on bad path / EINVAL.
fn statvfs(path: &str) -> Option<DiskState> {
    let c = CString::new(path).ok()?;
    // SAFETY: libc::statvfs writes into `buf` only on rc==0. CString is null-terminated.
    let mut buf: std::mem::MaybeUninit<libc::statvfs> = std::mem::MaybeUninit::uninit();
    let rc = unsafe { libc::statvfs(c.as_ptr() as *const c_char, buf.as_mut_ptr()) };
    if rc != 0 { return None; }
    let s = unsafe { buf.assume_init() };
    let block_size = s.f_frsize as u64;
    let total_bytes = (s.f_blocks as u64).saturating_mul(block_size);
    let avail_bytes = (s.f_bavail as u64).saturating_mul(block_size);
    Some(DiskState { total_bytes, avail_bytes })
}

/// Period the host uses to know when to recreate this backend on a config change.
pub fn poll_period() -> Duration { Duration::from_secs(POLL_SECS) }
