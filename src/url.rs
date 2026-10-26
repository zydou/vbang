//! The two percent-encoders, which are *not* interchangeable.
//!
//! A query is percent-encoded in exactly two situations, with a different
//! encoder each time, and getting these wrong is invisible until the day
//! someone searches for a string containing `&`.
//!
//! * the Google fallback runs the query through `uri_component`, a tight
//!   set, so `&`, `=`, `#`, `:` and friends all become `%XX`;
//! * a bang hit splices the query into a URL template with *no* encoding at
//!   all, and the only pass over the result is `encode_url`, whose effective
//!   safe set is wider: `#`, `$`, `&`, `?`, `=`, `:` and `@` survive.
//!
//! So `q=vy a&b=c` lands on `search_query=a&b=c` while `q=a&b=c` lands on
//! `google.com/...&q=a%26b%3Dc`. Both are this project's conventions and are
//! locked in by the tests below.

/// The characters `uri_component` leaves alone: RFC 3986's unreserved
/// characters, plus the sub-delims `!`, `*`, `'`, `(` and `)`. That is
/// `A-Za-z0-9`, `-`, `.`, `_`, `~` and those five. Everything else becomes
/// `%XX` in uppercase hex.
const fn is_component_safe(b: u8) -> bool {
    matches!(
        b,
        b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'.' | b'!' | b'*' | b'~' | b'\''
            | b'(' | b')' | b'_'
    )
}

/// The characters `encode_url` leaves alone inside a collected run. Wider than
/// the component set: `#`, `$` and every character of `; , / ? : @ & =`
/// survive, which is what makes a raw `#` in a bang query become a fragment
/// instead of `%23`.
const fn is_uri_safe(c: char) -> bool {
    matches!(
        c as u32,
        0x21 | 0x23 | 0x24 | 0x26..=0x3b | 0x3d | 0x3f | 0x40 | 0x41..=0x5a | 0x5f
            | 0x61..=0x7a | 0x7e
    )
}

/// The characters this matcher treats as already fine; a run of anything else
/// is what gets collected and percent-encoded.
///
/// The safe set, written out as the arms below: `!`, `%`, `&` through `;`,
/// `=`, `?` through `[`, `]`, `_`, `a` through `z`, `~`. Two traps: `=` (0x3d)
/// and `?` through `[` (0x3f..=0x5b) are written as separate arms, so `>`
/// (0x3e) sits in the gap between them and *does* get encoded; and the range
/// stops at `[` (0x5b) with `]` (0x5d) as its own arm, so `\` (0x5c) also falls
/// in a gap and gets encoded. Reading either gap as a range silently changes
/// the output.
const fn is_run_safe(c: char) -> bool {
    matches!(
        c as u32,
        0x21 | 0x25 | 0x26..=0x3b | 0x3d | 0x3f..=0x5b | 0x5d | 0x5f | 0x61..=0x7a | 0x7e
    )
}

const fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// `uri_component`: percent-encodes every UTF-8 byte outside the component
/// set, in uppercase hex.
pub fn uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if is_component_safe(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// How many characters the matcher consumes at `i`.
///
/// Three `%` cases: `%` plus a non-hex character consumes 2, `%` plus a hex
/// digit plus a non-hex digit consumes 3, and `%` at the very end consumes 1.
/// Anything else consumes 1 when it is not `is_run_safe`, and 0 otherwise — 0
/// means "this character is fine as it stands".
fn consumes(chars: &[char], i: usize) -> usize {
    if chars[i] == '%' {
        let n = chars.len();
        if i + 1 < n && !is_hex(chars[i + 1] as u8) {
            return 2;
        }
        if i + 2 < n && is_hex(chars[i + 1] as u8) && !is_hex(chars[i + 2] as u8) {
            return 3;
        }
        if i + 1 == n {
            return 1;
        }
        return 0;
    }
    usize::from(!is_run_safe(chars[i]))
}

/// `encode_url`: collect every run of characters the matcher flags and encode
/// each run whole against `is_uri_safe`.
///
/// Feeding a run *as a whole* is what makes `%20` survive while the second `%`
/// in `%%` does not: the run is delimited by the matcher, so the encoding never
/// sees across it. No lone-surrogate handling is needed — Rust strings are
/// valid UTF-8, so an unpaired surrogate can never reach this function.
pub fn encode_url(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;

    while i < chars.len() {
        if consumes(&chars, i) == 0 {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        let start = i;
        while i < chars.len() && consumes(&chars, i) > 0 {
            i += consumes(&chars, i);
        }
        for c in &chars[start..i] {
            if c.is_ascii() && is_uri_safe(*c) {
                out.push(*c);
            } else {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_component_is_the_component_set() {
        // One case per interesting byte, plus a final mixed string that pins
        // where the safe set's boundary falls.
        let cases = [
            (" ", "%20"),
            ("&", "%26"),
            ("=", "%3D"),
            ("#", "%23"),
            ("%", "%25"),
            ("'", "'"),
            ("\"", "%22"),
            ("<", "%3C"),
            ("\\", "%5C"),
            ("`", "%60"),
            ("+", "%2B"),
            ("(", "("),
            (")", ")"),
            ("!", "!"),
            ("*", "*"),
            ("/", "%2F"),
            ("@", "%40"),
            (":", "%3A"),
            ("~", "~"),
            ("?", "%3F"),
            (",", "%2C"),
            (";", "%3B"),
            ("$", "%24"),
            ("[", "%5B"),
            ("]", "%5D"),
            ("é", "%C3%A9"),
            ("你", "%E4%BD%A0"),
            (
                "#$<~?,/!*()'@:$[]_-\\",
                "%23%24%3C~%3F%2C%2F!*()'%40%3A%24%5B%5D_-%5C",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(uri_component(input), want, "uri_component({input:?})");
        }
    }

    #[test]
    fn encode_url_is_the_loose_encoder() {
        // `changed` is the plain single-byte encodings; `runs` is the
        // multi-character strings where a partial escape decides the output.
        let changed: [(&str, &str); 12] = [
            (" ", "%20"),
            ("\"", "%22"),
            ("<", "%3C"),
            (">", "%3E"),
            ("\\", "%5C"),
            ("^", "%5E"),
            ("`", "%60"),
            ("{", "%7B"),
            ("|", "%7C"),
            ("}", "%7D"),
            ("\t", "%09"),
            ("\n", "%0A"),
        ];
        let runs = [
            ("100%", "100%25"),
            ("50%50%", "50%50%25"),
            ("%%", "%25%25"),
            ("a%zz", "a%25zz"),
            ("a%2 0", "a%252%200"),
            ("a%2", "a%2"),
            ("%2", "%2"),
            ("p%0", "p%0"),
            ("%20", "%20"),
            ("100%25", "100%25"),
            ("100%2", "100%2"),
            ("100%ZZ", "100%25ZZ"),
            ("a%%b", "a%25%25b"),
            ("a%2%b", "a%252%25b"),
            ("a%2G", "a%252G"),
            ("x%YZ", "x%25YZ"),
            ("a%2%%", "a%252%25%25"),
            // A run is fed whole, so the `%` check stops at the end of the run
            // rather than the end of the string.
            ("%a%20", "%25a%2520"),
            // `%` plus a hex digit plus a non-hex digit consumes three
            // characters, which is what separates a truncated escape from a
            // valid one followed by text.
            ("%2 ", "%252%20"),
            ("%2z", "%252z"),
            ("x%Z ", "x%25Z%20"),
            ("a>b", "a%3Eb"),
            ("a< >b", "a%3C%20%3Eb"),
            ("`^\\{}", "%60%5E%5C%7B%7D"),
            ("' )", "'%20)"),
            ("#$<~?,/!*()'@:$[]_-\\", "#$%3C~?,/!*()'@:$[]_-%5C"),
            ("a~b", "a~b"),
            ("[x]y", "[x]y"),
            ("#&?=:$@", "#&?=:$@"),
            ("你", "%E4%BD%A0"),
            ("é", "%C3%A9"),
            ("😀", "%F0%9F%98%80"),
            ("你好世界", "%E4%BD%A0%E5%A5%BD%E4%B8%96%E7%95%8C"),
            ("%20你", "%20%E4%BD%A0"),
        ];
        for (input, want) in changed.iter().copied().chain(runs.iter().copied()) {
            assert_eq!(encode_url(input), want, "encode_url({input:?})");
        }

        // Boundary of the safe set, one byte per range end.
        for b in [
            0x21u8, 0x23, 0x24, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30,
            0x39, 0x3a, 0x3b, 0x3d, 0x3f, 0x40, 0x41, 0x5a, 0x5b, 0x5d, 0x5f, 0x61, 0x7a, 0x7e,
        ] {
            let s = char::from(b).to_string();
            assert_eq!(encode_url(&s), s, "encode_url should leave 0x{b:02x} alone");
        }

        // The gaps in that set are encoded: `>` (0x3e) sits between 0x3d and
        // 0x3f..=0x5b, and `\` (0x5c) sits between 0x5b and 0x5d.
        for b in [0x22u8, 0x3c, 0x3e, 0x5c, 0x5e, 0x60, 0x7b, 0x7c, 0x7d] {
            let s = char::from(b).to_string();
            assert_ne!(encode_url(&s), s, "encode_url should encode 0x{b:02x}");
        }

        // `%` is in the safe set, but only as the head of a complete escape: a
        // lone one, or one followed by a non-hex character, is encoded. A
        // truncated one at the very end of the string is left alone, because
        // the end-of-string case applies only to a `%` that is final.
        assert_eq!(encode_url("%"), "%25");
        assert_eq!(encode_url("%2"), "%2");
    }

    #[test]
    fn encode_url_is_idempotent() {
        for s in [
            "a b",
            "a&b=c#d",
            "100%",
            "100%25",
            "%20你好",
            "a%2G",
            "%a%20",
            "#$<~?,/!*()'@:$[]_-\\",
        ] {
            let once = encode_url(s);
            assert_eq!(encode_url(&once), once, "not idempotent on {s:?}");
        }
    }

    #[test]
    fn encode_url_preserves_uri_component_output() {
        // The reason the Google fallback can skip `encode_url` entirely:
        // `uri_component`'s output must already be safe for it, so the two
        // encoders never need to compose.
        for s in [
            "a b",
            "a&b=c#d",
            "100%",
            "100%25",
            "a%2b",
            "你好",
            "é",
            "'()!~*.-_",
            "&^=%",
        ] {
            let component = uri_component(s);
            assert_eq!(encode_url(&component), component, "on {s:?}");
        }
    }
}
