//! Notification data (G) — the GTK-free content of an `org.freedesktop.Notifications` `Notify`
//! call. The host's notification server (a zbus `#[interface]`, the SNI Watcher pattern) builds
//! these from incoming calls; the host renders them as popups (G1b). Pure data, like `TrayItem`.

/// The `urgency` hint (0/1/2). Critical never auto-expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    /// From the `urgency` hint byte (anything other than 0/2 is Normal, per the spec's leniency).
    pub fn from_hint(v: u8) -> Self {
        match v {
            0 => Urgency::Low,
            2 => Urgency::Critical,
            _ => Urgency::Normal,
        }
    }
}

/// Resolved `expire_timeout`: `-1` → server default, `0` → never, `>0` → that many milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    Default,
    Never,
    Millis(u32),
}

impl Timeout {
    pub fn from_spec(ms: i32) -> Self {
        match ms {
            ..=-1 => Timeout::Default,
            0 => Timeout::Never,
            n => Timeout::Millis(n as u32),
        }
    }
}

/// Why a notification closed — the `NotificationClosed` signal's `reason` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Expired,
    Dismissed,
    Closed,
    Other,
}

impl CloseReason {
    pub fn code(self) -> u32 {
        match self {
            CloseReason::Expired => 1,
            CloseReason::Dismissed => 2,
            CloseReason::Closed => 3,
            CloseReason::Other => 4,
        }
    }
}

/// One notification: a built `Notify` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    /// `app_icon` — a freedesktop icon name or a path. (Raw `image-data` hints are an image the host
    /// renders; G1b's concern.)
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// `(key, label)` action pairs; the `"default"` key fires on body click.
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
    pub timeout: Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_and_timeout_from_spec() {
        assert_eq!(Urgency::from_hint(0), Urgency::Low);
        assert_eq!(Urgency::from_hint(1), Urgency::Normal);
        assert_eq!(Urgency::from_hint(2), Urgency::Critical);
        assert_eq!(Urgency::from_hint(7), Urgency::Normal); // out-of-range → Normal

        assert_eq!(Timeout::from_spec(-1), Timeout::Default);
        assert_eq!(Timeout::from_spec(0), Timeout::Never);
        assert_eq!(Timeout::from_spec(5000), Timeout::Millis(5000));

        assert_eq!(CloseReason::Dismissed.code(), 2);
    }
}
