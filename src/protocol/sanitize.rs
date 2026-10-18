//! Sanitization utilities for LLM output cleaning and tag stripping.

pub const THINK_OPEN_TAGS: [&str; 4] = ["<think>", "<thinking>", "&lt;think&gt;", "&lt;thinking&gt;"];
pub const THINK_CLOSE_TAGS: [&str; 4] = ["</think>", "</thinking>", "&lt;/think&gt;", "&lt;/thinking&gt;"];
pub const RENDER_DELTA_CHUNK_BYTES: usize = 256;

/// Split UTF-8 string at a valid char boundary up to `max_bytes`.
pub fn split_utf8_prefix(text: &str, max_bytes: usize) -> (&str, &str) {
    if text.len() <= max_bytes {
        return (text, "");
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.split_at(end)
}

/// Find the earliest occurrence of any of the given tags in `text`.
/// Returns `Some((byte_offset, tag_length))`.
pub fn find_any_tag(text: &str, tags: &[&str]) -> Option<(usize, usize)> {
    let mut earliest: Option<(usize, usize)> = None;
    for &tag in tags {
        if let Some(pos) = text.find(tag) {
            match earliest {
                None => earliest = Some((pos, tag.len())),
                Some((cur_pos, _)) if pos < cur_pos => earliest = Some((pos, tag.len())),
                _ => {}
            }
        }
    }
    earliest
}

/// Strips raw system-leak tags like `<severity>...</severity>`, DSML tags, and leaked think tags.
pub fn strip_system_tags(text: &str) -> String {
    let mut s = text.to_string();

    // Strip <severity>...</severity>
    while let Some(start) = s.find("<severity>") {
        if let Some(end) = s[start..].find("</severity>") {
            s.replace_range(start..start + end + "</severity>".len(), "");
        } else {
            break;
        }
    }

    const TAGS_TO_REMOVE: [&str; 18] = [
        "</think>",
        "<think>",
        "</thinking>",
        "<thinking>",
        "&lt;/think&gt;",
        "&lt;think&gt;",
        "<｜DSML｜tool_calls>",
        "</｜DSML｜tool_calls>",
        "<|DSML|tool_calls>",
        "</|DSML|tool_calls>",
        "<｜DSML｜invoke>",
        "</｜DSML｜invoke>",
        "<|DSML|invoke>",
        "</|DSML|invoke>",
        "<｜DSML｜parameter>",
        "</｜DSML｜parameter>",
        "<|DSML|parameter>",
        "</|DSML|parameter>",
    ];

    for tag in &TAGS_TO_REMOVE {
        if s.contains(tag) {
            s = s.replace(tag, "");
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_any_tag() {
        let text = "Here is some thinking: </think> and then more text";
        let found = find_any_tag(text, &THINK_CLOSE_TAGS);
        assert_eq!(found, Some((23, 8))); // "</think>".len() == 8
    }

    #[test]
    fn test_strip_severity() {
        let text = "<severity>0</severity>Plan: Proceed with build.";
        assert_eq!(strip_system_tags(text), "Plan: Proceed with build.");
    }

    #[test]
    fn test_strip_dsml_and_think() {
        let text = "<think>internal</think><|DSML|tool_calls>visible";
        assert_eq!(strip_system_tags(text), "internalvisible");
    }

    #[test]
    fn test_split_utf8_prefix() {
        let text = "Xin chào các bạn!";
        let (first, rest) = split_utf8_prefix(text, 6);
        assert!(first.len() <= 6);
        assert_eq!(format!("{}{}", first, rest), text);
    }
}
