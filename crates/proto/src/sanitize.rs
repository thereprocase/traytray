//! Text sanitising for anything a shell will render or a toast will show.
//!
//! Apps are allowed to say anything, but not to *control* the display: no terminal escape
//! sequences, no C0/C1 control characters, no bidirectional overrides that could make one
//! string impersonate another. Newlines survive only where multi-line text is expected.

/// Characters that reorder or hide text (bidi embeddings/overrides/isolates, marks and
/// zero-width joiners used for spoofing).
fn is_format_control(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Strip control content and cap the length to `max_chars` characters. When `multiline` is
/// false, newlines and tabs become single spaces.
pub fn clean(input: &str, max_chars: usize, multiline: bool) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars * 4));
    let mut chars = input.chars().peekable();
    let mut count = 0usize;
    while let Some(c) = chars.next() {
        if count >= max_chars {
            break;
        }
        match c {
            // ESC starts an escape sequence. Drop the whole CSI/OSC sequence, not just ESC,
            // so "\x1b[31m" doesn't leave "[31m" behind.
            '\u{1B}' => skip_escape_sequence(&mut chars),
            '\n' if multiline => {
                out.push('\n');
                count += 1;
            }
            '\n' | '\t' | '\r' => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                    count += 1;
                }
            }
            c if c.is_control() || is_format_control(c) => {}
            c => {
                out.push(c);
                count += 1;
            }
        }
    }
    if chars.peek().is_some() && count >= max_chars {
        // Make truncation visible rather than silently cutting a sentence.
        out.pop();
        out.push('…');
    }
    out
}

fn skip_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek() {
        Some('[') => {
            chars.next();
            // CSI: parameters and intermediates, then one final byte in 0x40..=0x7E.
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7E}').contains(&c) {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            // OSC: terminated by BEL or ST (ESC \).
            while let Some(c) = chars.next() {
                if c == '\u{07}' {
                    break;
                }
                if c == '\u{1B}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        Some(_) => {
            // Two-character escape (e.g. ESC c): drop the next character too.
            chars.next();
        }
        None => {}
    }
}

/// Identifiers are opaque to the host but must be printable and bounded, because they are
/// echoed back to apps and used as map keys.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= crate::limits::MAX_ID_CHARS
        && id.chars().all(|c| c.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_sequences_entirely() {
        assert_eq!(clean("\u{1b}[31mred\u{1b}[0m text", 100, false), "red text");
    }

    #[test]
    fn strips_osc_hyperlink_sequences() {
        let s = "\u{1b}]8;;https://evil.example\u{7}click\u{1b}]8;;\u{7}";
        assert_eq!(clean(s, 100, false), "click");
    }

    #[test]
    fn strips_bracketed_paste_terminator() {
        assert_eq!(clean("a\u{1b}[201~b", 100, false), "ab");
    }

    #[test]
    fn strips_bidi_overrides() {
        // "Windows Security" spoof using a right-to-left override.
        assert_eq!(clean("abc\u{202E}fed", 100, false), "abcfed");
        assert_eq!(clean("a\u{2066}b\u{2069}c", 100, false), "abc");
    }

    #[test]
    fn strips_c0_and_c1_controls() {
        assert_eq!(clean("a\u{0}b\u{7}c\u{85}d\u{7f}e", 100, false), "abcde");
    }

    #[test]
    fn newlines_fold_to_spaces_unless_multiline() {
        assert_eq!(clean("one\ntwo\r\nthree", 100, false), "one two three");
        assert_eq!(clean("one\ntwo", 100, true), "one\ntwo");
    }

    #[test]
    fn caps_length_with_visible_ellipsis() {
        let s = clean(&"x".repeat(50), 10, false);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
        assert_eq!(clean("exactly10!", 10, false), "exactly10!");
    }

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(clean("ééééé", 5, false), "ééééé");
    }

    #[test]
    fn ids_must_be_printable_ascii() {
        assert!(valid_id("job-42"));
        assert!(!valid_id(""));
        assert!(!valid_id("has space"));
        assert!(!valid_id("é"));
        assert!(!valid_id(&"a".repeat(129)));
    }
}
