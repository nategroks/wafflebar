//! Hand-rolled `Exec=` tokenizer + field-code expansion per the fd.o Desktop Entry Spec §7.
//!
//! We do NOT use `freedesktop-desktop-entry::parse_exec` — it mis-handles `%i` (emits the icon
//! value as one arg instead of `--icon <value>`, two args) and tokenizes with
//! `split_ascii_whitespace`, which ignores the spec's double-quote rules. This module gets those
//! right and is exercised by the corner tests below.

use thiserror::Error;

/// Errors from expanding an `Exec=` value.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecError {
    /// No `Exec=` value, or it expanded to nothing.
    #[error("Exec value is missing or empty")]
    Empty,
    /// A double-quoted segment was never closed.
    #[error("unbalanced quote in Exec value")]
    UnbalancedQuote,
}

/// Tokenize an `Exec=` value into raw arguments, honoring the spec's double-quote + backslash
/// rules. Field codes (`%f` etc.) are preserved as their own tokens for [`expand`] to handle.
///
/// Spec: arguments are whitespace-separated; a `"`-quoted segment may contain spaces; inside
/// quotes, `\` escapes `"` `` ` `` `$` `\`. (The tokenizer is *not* a shell — no word-splitting
/// of `$VAR`, no globbing.)
fn tokenize(exec: &str) -> Result<Vec<String>, ExecError> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut started = false; // current token has content / was opened
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if started {
                    tokens.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnbalancedQuote),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n @ ('"' | '`' | '$' | '\\')) => cur.push(n),
                            Some(n) => {
                                cur.push('\\');
                                cur.push(n);
                            }
                            None => return Err(ExecError::UnbalancedQuote),
                        },
                        Some(other) => cur.push(other),
                    }
                }
            }
            other => {
                started = true;
                cur.push(other);
            }
        }
    }
    if started {
        tokens.push(cur);
    }
    Ok(tokens)
}

/// Expand a tokenized `Exec=` into final argv with field-code context.
///
/// Field codes (each appears as its own argument per spec):
/// - `%f`/`%u` → the first file/URI (nothing if none passed)
/// - `%F`/`%U` → all files/URIs
/// - `%i` → `--icon <icon>` (**two** args) when `icon` is set, **zero** args otherwise
/// - `%c` → the localized application name
/// - `%k` → the desktop-file path
/// - `%%` → a literal `%`
/// - deprecated `%d %D %n %N %v %m` → silently dropped (spec)
pub fn expand(
    exec: &str,
    icon: Option<&str>,
    name: Option<&str>,
    path: &str,
    files: &[String],
) -> Result<Vec<String>, ExecError> {
    let mut argv = Vec::new();
    for tok in tokenize(exec)? {
        match tok.as_str() {
            "%f" | "%u" => {
                if let Some(f) = files.first() {
                    argv.push(f.clone());
                }
            }
            "%F" | "%U" => argv.extend(files.iter().cloned()),
            "%i" => {
                if let Some(i) = icon {
                    argv.push("--icon".to_string());
                    argv.push(i.to_string());
                }
            }
            "%c" => {
                if let Some(n) = name {
                    argv.push(n.to_string());
                }
            }
            "%k" => argv.push(path.to_string()),
            // Deprecated codes: strip silently (do not error, do not warn).
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            // Any other token: collapse the `%%` literal-percent escape. (Field codes embedded
            // mid-token are non-conformant per spec and intentionally not expanded.)
            other => argv.push(other.replace("%%", "%")),
        }
    }
    if argv.is_empty() {
        return Err(ExecError::Empty);
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(exec: &str, files: &[&str]) -> Vec<String> {
        let files: Vec<String> = files.iter().map(|s| s.to_string()).collect();
        expand(exec, Some("firefox"), Some("Web Browser"), "/a/b.desktop", &files).unwrap()
    }

    #[test]
    fn icon_field_is_two_args_or_zero() {
        assert_eq!(ex("app %i", &[]), ["app", "--icon", "firefox"]);
        // no icon set -> zero args
        let v = expand("app %i", None, None, "/x", &[]).unwrap();
        assert_eq!(v, ["app"]);
    }

    #[test]
    fn c_is_localized_name_k_is_path() {
        assert_eq!(ex("app %c %k", &[]), ["app", "Web Browser", "/a/b.desktop"]);
    }

    #[test]
    fn single_vs_list_files() {
        assert_eq!(ex("app %f", &["one", "two"]), ["app", "one"]); // %f = first only
        assert_eq!(ex("app %F", &["one", "two"]), ["app", "one", "two"]);
        assert_eq!(ex("app %f", &[]), ["app"]); // none passed -> empty
        assert_eq!(ex("app %U", &[]), ["app"]);
    }

    #[test]
    fn deprecated_codes_silently_stripped() {
        assert_eq!(ex("app %d %D %n %N %v %m end", &[]), ["app", "end"]);
    }

    #[test]
    fn literal_percent() {
        assert_eq!(ex("app 100%%", &[]), ["app", "100%"]);
    }

    #[test]
    fn quoted_argument_with_spaces() {
        assert_eq!(
            ex(r#"/opt/foo "a b c" %f"#, &["f"]),
            ["/opt/foo", "a b c", "f"]
        );
    }

    #[test]
    fn backslash_escapes_inside_quotes() {
        assert_eq!(ex(r#"app "a\"b""#, &[]), ["app", "a\"b"]);
    }

    #[test]
    fn unbalanced_quote_errors() {
        assert_eq!(
            expand(r#"app "oops"#, None, None, "/x", &[]),
            Err(ExecError::UnbalancedQuote)
        );
    }

    #[test]
    fn empty_exec_errors() {
        assert_eq!(expand("", None, None, "/x", &[]), Err(ExecError::Empty));
    }
}
