//! Application-menu categorization (E1). Groups the launchable apps from
//! [`crate::list_applications`] into the freedesktop registered Main Categories, for the
//! applications menu (E2).
//!
//! We bucket by the `Categories=` key rather than parsing the Menu-Spec `.menu` XML: the main
//! buckets cover the user-visible value, while the full `.menu` tree (distro custom layouts,
//! `<Include>`/`<Exclude>` rules, merge order) is large and fragile to mirror and has no adopted
//! Rust crate. Deferred until a real consumer needs custom layouts — see docs/UPSTREAM.md.

use std::collections::HashMap;

use crate::freedesktop::DesktopApp;

/// Registered freedesktop Main Categories → display bucket. `Audio`/`Video` fold into `AudioVideo`
/// (the spec requires they co-occur with it). The first of an app's `Categories=` that appears here
/// wins, so `Network;FileTransfer` → Internet.
const CATEGORY_BUCKETS: &[(&str, &str)] = &[
    ("AudioVideo", "Multimedia"),
    ("Audio", "Multimedia"),
    ("Video", "Multimedia"),
    ("Development", "Development"),
    ("Education", "Education"),
    ("Game", "Games"),
    ("Graphics", "Graphics"),
    ("Network", "Internet"),
    ("Office", "Office"),
    ("Science", "Science"),
    ("Settings", "Settings"),
    ("System", "System"),
    ("Utility", "Accessories"),
];

/// Apps with no registered main category land here.
const OTHER: &str = "Other";

/// Display order of buckets in the menu (empty buckets are omitted).
const BUCKET_ORDER: &[&str] = &[
    "Multimedia",
    "Development",
    "Education",
    "Games",
    "Graphics",
    "Internet",
    "Office",
    "Science",
    "Settings",
    "System",
    "Accessories",
    OTHER,
];

/// The display bucket for an app: the first of its `Categories=` that is a registered main category,
/// else `Other`.
fn bucket_of(app: &DesktopApp) -> &'static str {
    for cat in &app.categories {
        if let Some(&(_, display)) =
            CATEGORY_BUCKETS.iter().find(|(key, _)| key.eq_ignore_ascii_case(cat))
        {
            return display;
        }
    }
    OTHER
}

/// Group apps into menu buckets in display order, omitting empty ones; apps within a bucket are
/// sorted by name (case-insensitive). Pure — call with the output of [`crate::list_applications`].
pub fn categorized(apps: Vec<DesktopApp>) -> Vec<(&'static str, Vec<DesktopApp>)> {
    let mut buckets: HashMap<&'static str, Vec<DesktopApp>> = HashMap::new();
    for app in apps {
        buckets.entry(bucket_of(&app)).or_default().push(app);
    }
    let mut out = Vec::new();
    for &name in BUCKET_ORDER {
        if let Some(mut apps) = buckets.remove(name) {
            apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            out.push((name, apps));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_registered_main_category_wins() {
        // Network is a main category, FileTransfer is not → Internet, regardless of order.
        assert_eq!(bucket_of(&DesktopApp::test("Transmission", &["Network", "FileTransfer"])), "Internet");
        assert_eq!(bucket_of(&DesktopApp::test("Transmission", &["FileTransfer", "Network"])), "Internet");
    }

    #[test]
    fn audio_video_fold_into_multimedia() {
        assert_eq!(bucket_of(&DesktopApp::test("mpv", &["AudioVideo", "Player"])), "Multimedia");
        assert_eq!(bucket_of(&DesktopApp::test("Audacity", &["Audio"])), "Multimedia");
    }

    #[test]
    fn uncategorized_goes_to_other() {
        assert_eq!(bucket_of(&DesktopApp::test("Weird", &[])), "Other");
        assert_eq!(bucket_of(&DesktopApp::test("Weird", &["FileTransfer"])), "Other");
    }

    #[test]
    fn categorized_orders_buckets_sorts_apps_and_omits_empty() {
        let apps = vec![
            DesktopApp::test("Zed", &["Development"]),
            DesktopApp::test("gimp", &["Graphics"]),
            DesktopApp::test("Audacity", &["Audio"]),
            DesktopApp::test("Blender", &["Graphics"]),
        ];
        let got = categorized(apps);
        let names: Vec<&str> = got.iter().map(|(b, _)| *b).collect();
        // Multimedia before Development before Graphics (display order); no empty buckets.
        assert_eq!(names, vec!["Multimedia", "Development", "Graphics"]);
        // Graphics apps sorted case-insensitively: Blender before gimp.
        let graphics = &got.iter().find(|(b, _)| *b == "Graphics").unwrap().1;
        assert_eq!(graphics.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["Blender", "gimp"]);
    }
}
