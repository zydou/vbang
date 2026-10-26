//! The whole product, minus the socket.
//!
//! Pure apart from `load_bangs`, which reads the bang table once at startup.
//! `resolve` takes the query string and that table and returns exactly what to
//! redirect to; the only HTTP thing here is a `StatusCode` carried out on its
//! way out. The layer in `main.rs` is a thin shell that binds a port and turns
//! a `Decision` into a response.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use axum::http::StatusCode;
use serde::Deserialize;

use crate::url::{encode_url, uri_component};

/// One entry of `user.json`: a bang alias `t`, and the destination URL `u` in
/// which `{{{s}}}` stands for the search terms.
#[derive(Debug, Clone, Deserialize)]
pub struct Bang {
    pub t: String,
    pub u: String,
}

/// Where the browser should go next.
///
/// The status travels with the URL so the 302/303 split stays next to the logic
/// that decides it, instead of being scattered across the handler: a bang hit
/// answers 302 and every fallback answers 303, and the handler never chooses
/// between them. The tests below lock both halves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Bang { url: String, status: StatusCode },
    Search { url: String, status: StatusCode },
}

/// A word character: `[A-Za-z0-9_]`, ASCII only.
///
/// It plays two roles in the bang pattern, and both roles use the *same* set —
/// which is why the whole matcher is a short loop and not a dependency. The
/// first is the run of one to eight word characters after the leading `v`. The
/// second is the trailing boundary test: a boundary exists when the next byte is
/// absent or is not a word character.
///
/// ASCII-only is load-bearing. A non-ASCII letter is not a word character, so
/// `vb你好` matches `vb`: the byte after `b` is 0xE4, which fails the test, so
/// there is a boundary there. The `regex` crate defaults to Unicode mode, where
/// `你` *is* a word character and `b|你` is not a boundary, so the same input
/// would silently fall through to the Google fallback. Testing one *byte*
/// against this set gives the intended behaviour directly: every byte of a
/// non-ASCII code point fails the test, so any of them is a boundary.
fn is_word_char(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z')
}

/// Find the bang at the start of `q` and return the match's end offset in
/// bytes: a `v` or `V`, then one to eight word characters, then a word
/// boundary.
///
/// The bound is eight. With six the shipped `user.json` entry `"flathub"`
/// (7 characters) can never match, and `user.json` *is* the product — a bang in
/// the table has to be reachable. Eight changes only one observable case
/// (`vflathub <q>`), because any other 7- or 8-character prefix is simply not in
/// the table and falls through to the Google fallback either way.
///
/// Longer than the bound fails to match *at all* rather than being truncated:
/// in `vaaaaaaaaa` the byte after the eighth word character is itself a word
/// character, so there is no boundary, and no shorter split of the run has one
/// either.
fn bang_end(q: &str) -> Option<usize> {
    let [first, ..] = q.as_bytes() else {
        return None;
    };
    if !matches!(first, b'V' | b'v') {
        return None;
    }
    let bytes = q.as_bytes();
    let mut end = 1;
    while end < 9 && end < bytes.len() && is_word_char(bytes[end]) {
        end += 1;
    }
    if end == 1 {
        return None; // at least one word character must follow the `v`
    }
    if end < bytes.len() && is_word_char(bytes[end]) {
        return None; // no word boundary after the run
    }
    Some(end)
}

/// Split `q` into the bang (without its leading `v`) and the remaining query.
///
/// With no bang the query is returned trimmed but otherwise untouched. Slicing
/// at the end of the match removes only the bang: the match is anchored at the
/// start of the string, so everything from the match end on is left in place
/// with its original spacing intact, and a second `v`-prefix later in the query
/// survives. Every byte consumed by `bang_end` is ASCII, so `end` always lands
/// on a character boundary.
pub fn split_bang(q: &str) -> (Option<&str>, &str) {
    match bang_end(q) {
        Some(end) => (Some(&q[1..end]), trim_ws(&q[end..])),
        None => (None, trim_ws(q)),
    }
}

/// Trim both ends. The set is exactly 25 code points: 0x0009-0x000D, 0x0020,
/// 0x00A0, 0x1680, 0x2000-0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000 and
/// 0xFEFF.
///
/// `str::trim` can not be used: it follows Unicode `White_Space`, which lacks
/// U+FEFF and includes U+0085, so against the set above it both misses the BOM
/// and eats the line separator. Both directions are pinned in the tests below.
fn trim_ws(s: &str) -> &str {
    let is_ws = |c: char| {
        matches!(
            c as u32,
            0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029
                | 0x202f | 0x205f | 0x3000 | 0xfeff
        )
    };
    let start = s
        .char_indices()
        .find(|(_, c)| !is_ws(*c))
        .map_or(s.len(), |(i, _)| i);
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ws(*c))
        .map_or(start, |(i, c)| i + c.len_utf8());
    &s[start..end]
}

/// The entire product.
///
/// `bangs` is consulted first-match on `t`: case-sensitive, and the first hit
/// wins. The substitution is deliberately *not* URI-encoded and replaces only
/// the first `{{{s}}}`. `$` in the terms is literal — `replacen` treats the
/// terms as text and interprets no markers — so `$&` or `$$` reach the URL as
/// typed, which is what a user typing `$` wants.
///
/// `encode_url` is the cleanup pass the bang path runs over its finished URL.
/// The Google fallback never calls it: `uri_component`'s output is already
/// safe for `encode_url`, which the tests in `url` lock in — so the two
/// encoders never have to compose.
pub fn resolve(q: &str, bangs: &[Bang]) -> Decision {
    let (bang, main_query) = split_bang(q);

    if let Some(entry) = bang.and_then(|bang| bangs.iter().find(|b| b.t == bang)) {
        let spliced = entry.u.replacen("{{{s}}}", main_query, 1);
        return Decision::Bang {
            url: encode_url(&spliced),
            status: StatusCode::FOUND,
        };
    }

    Decision::Search {
        url: format!(
            "https://www.google.com/search?newwindow=1&q={}",
            uri_component(trim_ws(q))
        ),
        status: StatusCode::SEE_OTHER,
    }
}

/// Why startup had to stop.
#[derive(Debug)]
pub enum LoadError {
    Io {
        path: String,
        error: std::io::Error,
    },
    Parse {
        path: String,
        error: serde_json::Error,
    },
    Duplicate {
        bang: String,
        index: usize,
    },
    Empty {
        index: usize,
        field: &'static str,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "cannot read bangs from {path}: {error}"),
            Self::Parse { path, error } => write!(f, "bangs file {path} is not valid: {error}"),
            Self::Duplicate { bang, index } => {
                write!(f, "duplicate bang {bang:?} at index {index}; delete one")
            }
            Self::Empty { index, field } => {
                write!(f, "entry {index} has an empty {field}; it can never work")
            }
        }
    }
}

/// Read and validate the bang table.
///
/// Every failure here is fatal by design: the file *is* the product, so a
/// broken bang table is a data error for the operator to fix, not a runtime
/// fault to paper over. The process restarts in milliseconds.
pub fn load_bangs(path: &Path) -> Result<Vec<Bang>, LoadError> {
    let raw = std::fs::read_to_string(path).map_err(|error| LoadError::Io {
        path: path.to_string_lossy().into_owned(),
        error,
    })?;
    let bangs: Vec<Bang> = serde_json::from_str(&raw).map_err(|error| LoadError::Parse {
        path: path.to_string_lossy().into_owned(),
        error,
    })?;

    let mut seen = HashSet::new();
    for (index, bang) in bangs.iter().enumerate() {
        // An empty `t` can never match (a bang is at least one character after
        // the `v`) and an empty `u` would answer with a blank `Location`, so
        // both are dead config rather than edge cases worth tolerating.
        if bang.t.is_empty() {
            return Err(LoadError::Empty { index, field: "t" });
        }
        if bang.u.is_empty() {
            return Err(LoadError::Empty { index, field: "u" });
        }
        if !seen.insert(bang.t.clone()) {
            return Err(LoadError::Duplicate {
                bang: bang.t.clone(),
                index,
            });
        }
    }
    Ok(bangs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bangs() -> Vec<Bang> {
        vec![
            Bang { t: "b".into(), u: "https://cn.bing.com/search?ensearch=1&q={{{s}}}".into() },
            Bang { t: "y".into(), u: "https://www.youtube.com/results?search_query={{{s}}}".into() },
            Bang { t: "gu".into(), u: "https://github.com/{{{s}}}".into() },
            Bang {
                t: "bn".into(),
                u: "https://github.com/search?q=repo%3Abinance%2Fbinance-connector-python+{{{s}}}&type=code".into(),
            },
            Bang { t: "flathub".into(), u: "https://flathub.org/apps/search?q={{{s}}}".into() },
            // Two placeholders: only the first is substituted.
            Bang {
                t: "x".into(),
                u: "https://example.org/p/{{{s}}}/mid/{{{s}}}/end".into(),
            },
            // No placeholder at all: the query is silently discarded.
            Bang { t: "fixed".into(), u: "https://example.org/fixed-path".into() },
        ]
    }

    fn bang_case(q: &str, want: &str) {
        let Decision::Bang { url, status } = resolve(q, &bangs()) else {
            panic!("expected a bang redirect for {q:?}");
        };
        assert_eq!(status, StatusCode::FOUND, "{q:?}");
        assert_eq!(url, want, "{q:?}");
    }

    fn search_case(q: &str, want: &str) {
        let Decision::Search { url, status } = resolve(q, &bangs()) else {
            panic!("expected a search redirect for {q:?}");
        };
        assert_eq!(status, StatusCode::SEE_OTHER, "{q:?}");
        assert_eq!(url, want, "{q:?}");
    }

    #[test]
    fn bang_hit_is_302() {
        bang_case("vb hello", "https://cn.bing.com/search?ensearch=1&q=hello");
        // A raw space becomes `%20`; this is `encode_url` doing its job.
        bang_case(
            "vb hello world",
            "https://cn.bing.com/search?ensearch=1&q=hello%20world",
        );
        // Paths, not query strings: the `+` already in the template survives.
        bang_case("vgu someuser", "https://github.com/someuser");
        bang_case(
            "vbn hello",
            "https://github.com/search?q=repo%3Abinance%2Fbinance-connector-python+hello&type=code",
        );
    }

    #[test]
    fn the_substitution_is_not_encoded() {
        // Spliced raw on purpose: `&` and `=` pass straight through and become
        // real parameters on the target site.
        bang_case(
            "vy a&b=c",
            "https://www.youtube.com/results?search_query=a&b=c",
        );
        bang_case("vy a+b", "https://www.youtube.com/results?search_query=a+b");
        bang_case(
            "vy %20hello",
            "https://www.youtube.com/results?search_query=%20hello",
        );
        bang_case(
            "vy 100%",
            "https://www.youtube.com/results?search_query=100%25",
        );
        bang_case(
            "vy 你好",
            "https://www.youtube.com/results?search_query=%E4%BD%A0%E5%A5%BD",
        );
        bang_case("vy ", "https://www.youtube.com/results?search_query=");
    }

    #[test]
    fn only_the_first_placeholder_is_substituted() {
        bang_case(
            "vx hello world",
            "https://example.org/p/hello%20world/mid/%7B%7B%7Bs%7D%7D%7D/end",
        );
    }

    #[test]
    fn dollar_is_literal_in_the_terms() {
        // The terms are literal text: `$` is not a marker, so `$&` and `$$`
        // reach the URL as typed. The placeholder that is never substituted
        // survives, and `encode_url` percent-encodes it.
        bang_case(
            "vx $&",
            "https://example.org/p/$&/mid/%7B%7B%7Bs%7D%7D%7D/end",
        );
        bang_case(
            "vx $$",
            "https://example.org/p/$$/mid/%7B%7B%7Bs%7D%7D%7D/end",
        );
        bang_case("vy $$", "https://www.youtube.com/results?search_query=$$");
    }

    #[test]
    fn a_template_without_a_placeholder_discards_the_query() {
        bang_case("vfixed hello", "https://example.org/fixed-path");
    }

    #[test]
    fn only_the_first_bang_token_is_stripped() {
        bang_case(
            "vy vy bar",
            "https://www.youtube.com/results?search_query=vy%20bar",
        );
        bang_case(
            "vy vb foo",
            "https://www.youtube.com/results?search_query=vb%20foo",
        );
    }

    #[test]
    fn an_empty_remainder_is_fine() {
        bang_case("vy", "https://www.youtube.com/results?search_query=");
    }

    #[test]
    fn a_missing_or_unknown_bang_is_a_303_google_fallback() {
        search_case(
            "hello world",
            "https://www.google.com/search?newwindow=1&q=hello%20world",
        );
        search_case(
            "VB hello",
            "https://www.google.com/search?newwindow=1&q=VB%20hello",
        );
        search_case(
            "vz nothere",
            "https://www.google.com/search?newwindow=1&q=vz%20nothere",
        );
        search_case("", "https://www.google.com/search?newwindow=1&q=");
    }

    #[test]
    fn the_google_fallback_is_fully_encoded() {
        // The same characters that stay raw on the bang path are encoded here.
        search_case(
            "a&b=c#d",
            "https://www.google.com/search?newwindow=1&q=a%26b%3Dc%23d",
        );
        search_case(
            "#$<~?,/!*()'@:$[]_-\\",
            "https://www.google.com/search?newwindow=1&q=%23%24%3C~%3F%2C%2F!*()'%40%3A%24%5B%5D_-%5C",
        );
    }

    #[test]
    fn a_bang_must_be_at_the_start() {
        search_case(
            "foo vy bar",
            "https://www.google.com/search?newwindow=1&q=foo%20vy%20bar",
        );
        search_case(
            " vy hello",
            "https://www.google.com/search?newwindow=1&q=vy%20hello",
        );
        search_case(
            "\tvy hello",
            "https://www.google.com/search?newwindow=1&q=vy%20hello",
        );
    }

    #[test]
    fn the_bang_bound_is_enforced_by_a_word_boundary() {
        // Exactly 8 alphanumerics after `v` matches; 9 does not match at all.
        bang_case(
            "vflathub search",
            "https://flathub.org/apps/search?q=search",
        );
        search_case(
            "vabcdefghi x",
            "https://www.google.com/search?newwindow=1&q=vabcdefghi%20x",
        );
        search_case(
            "vpythonic",
            "https://www.google.com/search?newwindow=1&q=vpythonic",
        );
    }

    #[test]
    fn the_bang_class_is_word_characters() {
        // `_` is a word character, so `v_` matches the bang `_`, and `vz`
        // matches the bang `z`; neither is in the table, so both fall through
        // to the Google fallback.
        search_case("v_ x", "https://www.google.com/search?newwindow=1&q=v_%20x");
        search_case("vz x", "https://www.google.com/search?newwindow=1&q=vz%20x");
        // `[` is not a word character, so the run after `v` is empty and the
        // minimum of one is not met: nothing is stripped, and the whole query
        // goes to the fallback encoded.
        search_case(
            "v[ x",
            "https://www.google.com/search?newwindow=1&q=v%5B%20x",
        );
        search_case(
            "v[abc x",
            "https://www.google.com/search?newwindow=1&q=v%5Babc%20x",
        );
    }

    #[test]
    fn no_bang_at_all() {
        search_case("v", "https://www.google.com/search?newwindow=1&q=v");
        search_case("v x", "https://www.google.com/search?newwindow=1&q=v%20x");
    }

    #[test]
    fn a_non_ascii_letter_after_the_bang_is_a_boundary() {
        // Word characters are `[A-Za-z0-9_]`, and a boundary exists when the
        // next byte is absent or is not a word character. `is_word_char` tests
        // one byte against that set, so every byte of a non-ASCII code point
        // fails it and counts as a boundary — `vb你好` matches `vb`. CJK queries
        // often have no separating space.
        bang_case(
            "vb你好",
            "https://cn.bing.com/search?ensearch=1&q=%E4%BD%A0%E5%A5%BD",
        );
        bang_case("vyé", "https://www.youtube.com/results?search_query=%C3%A9");
        // A non-ASCII letter is still not a word character, so the matched run
        // ends there; the unknown bang falls through to the Google fallback.
        search_case(
            "vzé x",
            "https://www.google.com/search?newwindow=1&q=vz%C3%A9%20x",
        );
    }

    #[test]
    fn trim_is_not_str_trim() {
        assert_eq!(split_bang("vy   "), (Some("y"), ""));
        // A leading space kills the anchor, so this is not a bang.
        assert_eq!(split_bang("  vy  hi  "), (None, "vy  hi"));
        // U+FEFF is one of the 25 trim code points; a leading one kills the
        // anchor the way any leading whitespace does.
        assert_eq!(split_bang("vy\u{FEFF}hi"), (Some("y"), "hi"));
        assert_eq!(split_bang("\u{FEFF}vy hi"), (None, "vy hi"));
        // U+0085 is in Unicode `White_Space`, which `str::trim` strips, but not
        // in this set, so it survives.
        assert_eq!(split_bang("vy\u{0085}hi"), (Some("y"), "\u{0085}hi"));
        assert_eq!(split_bang("vy\u{00A0}hi"), (Some("y"), "hi"));
        assert_eq!(split_bang("vy\u{3000}hi"), (Some("y"), "hi"));
        // Non-ASCII interior text is untouched.
        assert_eq!(split_bang("vy 你好世界 "), (Some("y"), "你好世界"));
    }

    #[test]
    fn lookup_is_first_match_and_case_sensitive() {
        let bangs = vec![
            Bang {
                t: "b".into(),
                u: "https://first.example/".into(),
            },
            Bang {
                t: "b".into(),
                u: "https://second.example/".into(),
            },
        ];
        let Decision::Bang { url, status } = resolve("vb x", &bangs) else {
            panic!("expected a bang redirect");
        };
        assert_eq!(url, "https://first.example/");
        assert_eq!(status, StatusCode::FOUND);

        // `VB x` yields the bang `B`, which does not match `b`.
        search_case("VB x", "https://www.google.com/search?newwindow=1&q=VB%20x");
    }

    #[test]
    fn load_bangs_accepts_the_repo_file() {
        let bangs = load_bangs(Path::new("user.json")).expect("user.json should parse");
        assert!(
            bangs.len() >= 50,
            "expected the full shipped table, got {}",
            bangs.len()
        );
        assert!(bangs.iter().any(|b| b.t == "flathub"));
    }

    #[test]
    fn load_bangs_reports_a_missing_file() {
        let error = load_bangs(Path::new("../does-not-exist.json")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("cannot read"), "{message}");
    }

    #[test]
    fn load_bangs_reports_bad_json() {
        let path = std::env::temp_dir().join("search-rs-bad.json");
        std::fs::write(&path, "[{ \"t\": \"b\", }]").unwrap();
        let error = load_bangs(&path).unwrap_err();
        assert!(
            matches!(error, LoadError::Parse { .. }),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn load_bangs_reports_a_duplicate() {
        let path = std::env::temp_dir().join("search-rs-dup.json");
        std::fs::write(
            &path,
            "[{\"t\":\"b\",\"u\":\"x\"},{\"t\":\"b\",\"u\":\"y\"}]",
        )
        .unwrap();
        let error = load_bangs(&path).unwrap_err();
        match error {
            LoadError::Duplicate { bang, index } => {
                assert_eq!(bang, "b");
                assert_eq!(index, 1);
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
    }

    #[test]
    fn load_bangs_reports_an_empty_field() {
        for (body, field) in [
            ("[{\"t\":\"\",\"u\":\"x\"}]", "t"),
            ("[{\"t\":\"b\",\"u\":\"\"}]", "u"),
        ] {
            let path = std::env::temp_dir().join(format!("search-rs-empty-{field}.json"));
            std::fs::write(&path, body).unwrap();
            match load_bangs(&path) {
                Err(LoadError::Empty { index, field: got }) => {
                    assert_eq!(index, 0);
                    assert_eq!(got, field);
                }
                other => panic!("expected Empty({field}), got {other:?}"),
            }
        }
    }

    #[test]
    fn a_location_never_carries_a_raw_control_character() {
        // The URL goes into a `Location` header, and HTTP forbids CR and LF
        // there. The bang path is guarded by `encode_url` and the Google
        // fallback by `uri_component`, so assert the invariant on whichever arm
        // answered instead of trusting one encoder.
        for q in [
            "vy a\tb",
            "vy a\nb",
            "vy a\r\nX-Injected: 1",
            "vy a\rb",
            "vy \x1f\x7f",
            "vy 你好",
            "a\tb",
            "a\nb",
            "a\r\nX-Injected: 1",
            "a\rb",
            " \x1f\x7f",
        ] {
            let (Decision::Bang { url, .. } | Decision::Search { url, .. }) = resolve(q, &bangs());
            assert!(
                !url.bytes().any(|b| b <= 0x20 || b == 0x7f),
                "control character survived in {url:?}"
            );
        }
    }
}
