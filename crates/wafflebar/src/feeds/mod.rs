//! External feeds — the **second** input channel (separate from compositor IPC).
//!
//! Each module here listens on its own transport (UNIX socket, FIFO, …) for [`FeedEvent`]s and
//! hands them to plugins via the same event-loop seam as the WM backend. Today the only feed is
//! [`someblocks`] (the dwlb-compatible status-blocks stream); future feeds plug in next to it.
//!
//! **Why this lives at all:** the natewm contract demands two distinct inputs (`dwl -s` stdout
//! on stdin **and** clock/mem blocks somewhere else). Collapsing them into one stream is the
//! dwlb near-miss described in `docs/NATEWM_MODE.md`. Keeping feeds in their own module makes
//! that separation structural — adding a new feed cannot accidentally merge into the WM path.
//!
//! **Scaffold scope (this commit):** module declaration + [`someblocks`] skeleton. Transport
//! plumbing, parser, and bus wiring land in step 2.
//!
//! [`FeedEvent`]: wafflebar_core::FeedEvent

pub mod someblocks;
