//! Unified diffs of desired against current content. Output can contain
//! secrets: only the explicit `diff` commands call this.

use similar::TextDiff;

/// Unified diff from `current` (what is deployed) to `desired`. `None`
/// means the side does not exist. Empty when both are equal.
pub fn render(label: &str, current: Option<&[u8]>, desired: Option<&[u8]>) -> String {
    if current == desired {
        return String::new();
    }
    let old_label = current.map_or("/dev/null".to_owned(), |_| format!("a/{label}"));
    let new_label = desired.map_or("/dev/null".to_owned(), |_| format!("b/{label}"));
    let (Some(old), Some(new)) = (as_text(current), as_text(desired)) else {
        return format!("Binary files {old_label} and {new_label} differ\n");
    };
    TextDiff::from_lines(old, new)
        .unified_diff()
        .header(&old_label, &new_label)
        .to_string()
}

/// True when the sides differ and `render` summarizes them as binary.
pub fn is_binary(current: Option<&[u8]>, desired: Option<&[u8]>) -> bool {
    current != desired && (as_text(current).is_none() || as_text(desired).is_none())
}

/// Text of a side, `""` when absent, `None` when binary.
fn as_text(bytes: Option<&[u8]>) -> Option<&str> {
    let bytes = bytes.unwrap_or_default();
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_content_has_no_diff() {
        assert_eq!(render("~/.x", Some(b"a\n"), Some(b"a\n")), "");
        assert_eq!(render("~/.x", None, None), "");
    }

    #[test]
    fn modified_text_is_unified_diff() {
        let d = render("~/.x", Some(b"a\nold\n"), Some(b"a\nnew\n"));
        assert!(d.starts_with("--- a/~/.x\n+++ b/~/.x\n"), "{d}");
        assert!(
            d.contains("-old\n") && d.contains("+new\n") && d.contains(" a\n"),
            "{d}"
        );
    }

    #[test]
    fn missing_destination_is_all_added() {
        let d = render("~/.x", None, Some(b"one\n"));
        assert!(d.starts_with("--- /dev/null\n+++ b/~/.x\n"), "{d}");
        assert!(d.contains("+one\n"), "{d}");
    }

    #[test]
    fn missing_desired_is_all_removed() {
        let d = render("~/.x", Some(b"one\n"), None);
        assert!(d.contains("+++ /dev/null") && d.contains("-one\n"), "{d}");
    }

    #[test]
    fn binary_content_is_summarized() {
        let d = render("~/.x", Some(b"a\0b"), Some(b"a\0c"));
        assert_eq!(d, "Binary files a/~/.x and b/~/.x differ\n");
        let d = render("~/.x", Some(&[0xff, 0xfe]), Some(b"text"));
        assert!(d.starts_with("Binary files"), "{d}");
        assert!(is_binary(Some(&[0xff, 0xfe]), Some(b"text")));
        assert!(!is_binary(Some(b"a\0b"), Some(b"a\0b")), "equal sides do not differ");
        assert!(!is_binary(None, Some(b"text")));
    }

    #[test]
    fn missing_final_newline_is_marked() {
        let d = render("~/.x", Some(b"a"), Some(b"a\n"));
        assert!(d.contains("No newline at end of file"), "{d}");
    }
}
