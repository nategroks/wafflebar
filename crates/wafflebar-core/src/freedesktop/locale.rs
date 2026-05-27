//! Locale match chain per the fd.o Desktop Entry Spec §5 ("Localized values for keys").
//!
//! `freedesktop-desktop-entry::get_languages_from_env()` is non-spec (reads only `$LANG`, keeps
//! the `.UTF-8` encoding suffix, builds no fallback chain), so we build the chain here and pass
//! it to the crate's locale-aware accessors. For a value `lang_COUNTRY.ENCODING@MODIFIER` the
//! match order (most specific first) is: `lang_COUNTRY@MODIFIER`, `lang_COUNTRY`, `lang@MODIFIER`,
//! `lang`. Encoding is stripped and never matched.

/// Build the match chain (most-specific first) from a raw locale value like `en_US.UTF-8`.
pub fn locale_chain(raw: &str) -> Vec<String> {
    // Split off @MODIFIER, then .ENCODING, leaving `lang[_COUNTRY]`.
    let (before_mod, modifier) = match raw.split_once('@') {
        Some((b, m)) => (b, Some(m)),
        None => (raw, None),
    };
    let base = before_mod.split('.').next().unwrap_or(before_mod); // strip .ENCODING
    let (lang, country) = match base.split_once('_') {
        Some((l, c)) => (l, Some(c)),
        None => (base, None),
    };
    if lang.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    match (country, modifier) {
        (Some(c), Some(m)) => {
            out.push(format!("{lang}_{c}@{m}"));
            out.push(format!("{lang}_{c}"));
            out.push(format!("{lang}@{m}"));
            out.push(lang.to_string());
        }
        (Some(c), None) => {
            out.push(format!("{lang}_{c}"));
            out.push(lang.to_string());
        }
        (None, Some(m)) => {
            out.push(format!("{lang}@{m}"));
            out.push(lang.to_string());
        }
        (None, None) => out.push(lang.to_string()),
    }
    out
}

/// The chain from the environment, per spec precedence: `$LC_ALL` > `$LC_MESSAGES` > `$LANG`.
/// Empty (→ unlocalized default) when unset or `C`/`POSIX`.
pub fn from_env() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .filter(|s| !s.is_empty());
    match raw.as_deref() {
        Some("C") | Some("POSIX") | None => Vec::new(),
        Some(v) => locale_chain(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_then_lang() {
        assert_eq!(locale_chain("en_US.UTF-8"), ["en_US", "en"]);
    }

    #[test]
    fn lang_only() {
        assert_eq!(locale_chain("fr"), ["fr"]);
    }

    #[test]
    fn full_modifier_chain() {
        assert_eq!(
            locale_chain("sr_RS.UTF-8@latin"),
            ["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
    }

    #[test]
    fn lang_with_modifier_no_country() {
        assert_eq!(locale_chain("ca@valencia"), ["ca@valencia", "ca"]);
    }
}
