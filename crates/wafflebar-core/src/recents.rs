//! Frecency-ranked recently-launched applications (E1), for the applications menu's "recent"
//! section.
//!
//! Frequency × recency with a 30-day half-life: recent launches rank above stale ones without
//! discarding frequently-used apps. Persisted to `$XDG_DATA_HOME/wafflebar/recents.toml`, keyed by
//! **desktop-file id** (e.g. `firefox` — stable across reinstalls, unlike the path). Capped so the
//! file stays bounded; lowest-scoring entries are evicted past the cap (no arbitrary expiry — let
//! frecency rank the long tail).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const CAP: usize = 50;
const HALF_LIFE_DAYS: f64 = 30.0;

#[derive(Default, Serialize, Deserialize)]
pub struct Recents {
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    /// Desktop-file id (the stable key — never the path).
    id: String,
    launches: u32,
    /// Unix epoch seconds of the most recent launch.
    last_launch: u64,
}

impl Recents {
    /// Load from the standard path; a missing or corrupt file yields an empty list (the caller can
    /// warn). Corrupt-means-fresh rather than failing — a bad recents file must never block the menu.
    pub fn load() -> Self {
        Self::path().map(Self::load_from).unwrap_or_default()
    }

    /// Record a launch of `id` (increment its count, stamp now), evict past the cap, and persist.
    pub fn record_launch(&mut self, id: &str) -> std::io::Result<()> {
        self.bump(id, now_secs());
        match Self::path() {
            Some(p) => self.save_to(&p),
            None => Ok(()),
        }
    }

    /// The top `limit` app ids by frecency, highest first.
    pub fn ranked(&self, limit: usize) -> Vec<String> {
        self.ranked_at(limit, now_secs())
    }

    // --- internals, split out so the frecency/eviction logic is testable without the clock or disk ---

    fn bump(&mut self, id: &str, now: u64) {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.launches = e.launches.saturating_add(1);
                e.last_launch = now;
            }
            None => self.entries.push(Entry { id: id.to_string(), launches: 1, last_launch: now }),
        }
        if self.entries.len() > CAP {
            self.entries.sort_by(|a, b| score(b, now).total_cmp(&score(a, now)));
            self.entries.truncate(CAP);
        }
    }

    fn ranked_at(&self, limit: usize, now: u64) -> Vec<String> {
        let mut refs: Vec<&Entry> = self.entries.iter().collect();
        refs.sort_by(|a, b| score(b, now).total_cmp(&score(a, now)));
        refs.into_iter().take(limit).map(|e| e.id.clone()).collect()
    }

    fn load_from(path: PathBuf) -> Self {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_to(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(base.join("wafflebar").join("recents.toml"))
    }
}

/// Frecency: launch count decayed by age, 30-day half-life.
fn score(e: &Entry, now: u64) -> f64 {
    let age_days = now.saturating_sub(e.last_launch) as f64 / 86_400.0;
    e.launches as f64 * 2f64.powf(-age_days / HALF_LIFE_DAYS)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn bump_increments_existing_and_adds_new() {
        let mut r = Recents::default();
        r.bump("a", 100);
        r.bump("a", 200);
        r.bump("b", 200);
        assert_eq!(r.entries.len(), 2);
        let a = r.entries.iter().find(|e| e.id == "a").unwrap();
        assert_eq!(a.launches, 2);
        assert_eq!(a.last_launch, 200);
    }

    #[test]
    fn recency_breaks_ties_and_frequency_outweighs_stale() {
        let now = 1000 * DAY;
        let mut r = Recents::default();
        // Equal launches: the more recent ranks first.
        r.bump("recent", now - DAY);
        r.bump("old", now - 60 * DAY);
        assert_eq!(r.ranked_at(2, now), vec!["recent", "old"]);

        // Frequency outweighs age within reason: 10 launches a week ago beats 1 launch today.
        let mut r2 = Recents::default();
        r2.entries.push(Entry { id: "frequent".into(), launches: 10, last_launch: now - 7 * DAY });
        r2.entries.push(Entry { id: "once".into(), launches: 1, last_launch: now });
        assert_eq!(r2.ranked_at(1, now), vec!["frequent"]);
    }

    #[test]
    fn cap_evicts_lowest_scoring() {
        let now = 1000 * DAY;
        let mut r = Recents::default();
        for i in 0..CAP as u64 {
            // Older + single-launch entries; all low score.
            r.entries.push(Entry { id: format!("old{i}"), launches: 1, last_launch: now - 100 * DAY });
        }
        r.bump("fresh", now); // pushes over CAP → an old one is evicted, fresh stays
        assert_eq!(r.entries.len(), CAP);
        assert!(r.entries.iter().any(|e| e.id == "fresh"));
    }

    #[test]
    fn save_load_round_trips() {
        let mut r = Recents::default();
        r.bump("firefox", 500);
        r.bump("firefox", 600);
        let path = std::env::temp_dir().join(format!("wfb-recents-{}.toml", std::process::id()));
        r.save_to(&path).unwrap();
        let back = Recents::load_from(path.clone());
        assert_eq!(back.ranked(10), vec!["firefox"]);
        let e = back.entries.iter().find(|e| e.id == "firefox").unwrap();
        assert_eq!(e.launches, 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let path = std::env::temp_dir().join(format!("wfb-recents-bad-{}.toml", std::process::id()));
        std::fs::write(&path, "this is not valid toml {{{").unwrap();
        assert!(Recents::load_from(path.clone()).entries.is_empty());
        std::fs::remove_file(&path).ok();
    }
}
