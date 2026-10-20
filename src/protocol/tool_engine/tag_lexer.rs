//! Deterministic byte scanner for tool-call envelopes.
//!
//! This module deliberately contains no regular expressions.  Every function
//! scans the input once, which makes the behaviour identical for a complete
//! payload and for a payload split into arbitrary chunks.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch {
    /// Canonical tag family: `tool_call`, `function_call`, `invoke`, `action`
    /// or `dsml`.
    pub tag_name: &'static str,
    /// Tool name embedded in the opening tag (invoke/action/function_call).
    pub name: Option<String>,
    /// Byte offset of the opening `<`.
    pub start: usize,
    /// Byte offset immediately after the opening `>`.
    pub content_start: usize,
    /// The matching closing tag.
    pub close_tag: &'static str,
}

impl TagMatch {
    pub fn open_tag_len(&self) -> usize {
        self.content_start.saturating_sub(self.start)
    }
}

/// Opening/closing tag pairs. The order is not significant; the scanner always
/// selects the earliest occurrence in the input.
pub const TOOL_TAG_PAIRS: [(&str, &str, &str); 4] = [
    ("<tool_call", "</tool_call>", "tool_call"),
    ("<function_call", "</function_call>", "function_call"),
    ("<invoke", "</invoke>", "invoke"),
    ("<action", "</action>", "action"),
];

pub const DSML_TAG_PAIRS: [(&str, &str); 2] = [
    ("<｜tool call begin｜>", "<｜tool call end｜>"),
    ("<|tool call begin|>", "<|tool call end|>"),
];

const ALL_OPEN_TAGS: [&str; 6] = [
    "<tool_call",
    "<function_call",
    "<invoke",
    "<action",
    "<｜tool call begin｜>",
    "<|tool call begin|>",
];

const ALL_CLOSE_TAGS: [&str; 6] = [
    "</tool_call>",
    "</function_call>",
    "</invoke>",
    "</action>",
    "<｜tool call end｜>",
    "<|tool call end|>",
];

/// Find the first complete opening tag in `text`.
pub fn scan_tool_tag_start(text: &str) -> Option<TagMatch> {
    let mut best: Option<TagMatch> = None;

    for (open, close, family) in TOOL_TAG_PAIRS {
        let mut search_from = 0usize;
        while let Some(rel) = text[search_from..].find(open) {
            let pos = search_from + rel;
            let after = pos + open.len();
            if !is_tag_boundary(text, after) {
                search_from = pos + 1;
                continue;
            }
            if let Some(gt_rel) = text[after..].find('>') {
                let content_start = after + gt_rel + 1;
                let name = parse_name_attr(&text[pos..content_start]);
                let candidate = TagMatch {
                    tag_name: family,
                    name,
                    start: pos,
                    content_start,
                    close_tag: close,
                };
                if best.as_ref().map(|b| pos < b.start).unwrap_or(true) {
                    best = Some(candidate);
                }
            }
            break;
        }
    }

    for (open, close) in DSML_TAG_PAIRS {
        if let Some(pos) = text.find(open) {
            let candidate = TagMatch {
                tag_name: "dsml",
                name: None,
                start: pos,
                content_start: pos + open.len(),
                close_tag: close,
            };
            if best.as_ref().map(|b| pos < b.start).unwrap_or(true) {
                best = Some(candidate);
            }
        }
    }

    best
}

/// Find a tool tag start that is either complete or is a partial prefix at the
/// end of the current buffer.  The returned match is only complete when
/// `content_start <= text.len()`.
pub fn scan_partial_tool_tag_start(text: &str) -> Option<TagMatch> {
    if let Some(found) = scan_tool_tag_start(text) {
        return Some(found);
    }
    let hold = partial_tag_prefix_len(text);
    if hold == 0 {
        return None;
    }
    let start = text.len().saturating_sub(hold);
    Some(TagMatch {
        tag_name: "partial",
        name: None,
        start,
        content_start: text.len(),
        close_tag: "",
    })
}

/// Byte offset of the matching closing tag for `open_tag`.
///
/// `open_tag` may be either a canonical tag name (`tool_call`) or the literal
/// opening tag (`<tool_call>`).
pub fn scan_tool_tag_end(text: &str, open_tag: &str) -> Option<usize> {
    let close = close_tag_for(open_tag)?;
    text.find(close)
}

/// Longest suffix of `text` that is a strict prefix of any known tag.  This is
/// the amount of text that must be held back while streaming.
pub fn partial_tag_prefix_len(text: &str) -> usize {
    let mut longest = 0usize;
    for tag in ALL_OPEN_TAGS.iter().chain(ALL_CLOSE_TAGS.iter()) {
        // An opening prefix without its closing `>` (for example the full
        // `<tool_call` string) must still be held back; `scan_tool_tag_start`
        // only recognises tags that already contain `>`.
        for prefix_len in (1..=tag.len()).rev() {
            if !tag.is_char_boundary(prefix_len) {
                continue;
            }
            if text.ends_with(&tag[..prefix_len]) {
                longest = longest.max(prefix_len);
                break;
            }
        }
    }
    longest
}

/// True when `text` contains any tool envelope (complete or not).
pub fn contains_tool_tag(text: &str) -> bool {
    ALL_OPEN_TAGS.iter().any(|t| text.contains(t))
        || ALL_CLOSE_TAGS.iter().any(|t| text.contains(t))
        || text.contains("<｜tool sep｜>")
        || text.contains("<|tool sep|>")
}

/// Remove complete tool envelopes and stray closing tags.  An unclosed opening
/// tag truncates the remaining text, matching the streaming behaviour.
pub fn strip_tool_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while cursor < text.len() {
        let rest = &text[cursor..];
        match scan_tool_tag_start(rest) {
            Some(found) => {
                out.push_str(&rest[..found.start]);
                let after_open = found.content_start;
                if found.close_tag.is_empty() {
                    break;
                }
                if let Some(rel_end) = rest[after_open..].find(found.close_tag) {
                    let end = after_open + rel_end + found.close_tag.len();
                    cursor += end;
                } else {
                    // Unclosed envelope: drop the rest.
                    cursor = text.len();
                }
            }
            None => {
                // Also drop a dangling partial opening tag at the very end.
                let hold = partial_tag_prefix_len(rest);
                if hold > 0 {
                    out.push_str(&rest[..rest.len().saturating_sub(hold)]);
                    cursor = text.len();
                } else {
                    out.push_str(rest);
                    cursor = text.len();
                }
            }
        }
    }

    // Stray closing tags and DSML separators.
    for tag in ALL_CLOSE_TAGS.iter().chain(
        [
            "<｜tool calls begin｜>",
            "<｜tool calls end｜>",
            "<|tool calls begin|>",
            "<|tool calls end|>",
            "<｜tool sep｜>",
            "<|tool sep|>",
        ]
        .iter(),
    ) {
        out = out.replace(tag, "");
    }

    out.trim().to_string()
}

/// Streaming-safe end offset of visible text.
pub fn streamable_text_end(text: &str) -> usize {
    if let Some(found) = scan_tool_tag_start(text) {
        return found.start;
    }

    // Markdown fences are legacy compatibility markers. They are held back so
    // the compatibility extractor can see a complete fence.
    for marker in ["```bash", "```sh", "```shell", "```zsh", "```json"] {
        if let Some(pos) = text.find(marker) {
            return pos;
        }
    }

    let hold = partial_tag_prefix_len(text);
    if hold > 0 {
        return text.len().saturating_sub(hold);
    }

    // A trailing partial code fence is also held back.
    for marker in ["```bash", "```sh", "```shell", "```zsh", "```json"] {
        for prefix_len in (1..marker.len()).rev() {
            if text.ends_with(&marker[..prefix_len]) {
                return text.len().saturating_sub(prefix_len);
            }
        }
    }

    text.len()
}

/// Extract the value of a `name="..."` (or `name='...'`) attribute from an
/// opening tag.
pub fn parse_name_attr(open_tag: &str) -> Option<String> {
    parse_attr(open_tag, "name")
}

pub fn parse_attr(open_tag: &str, attr: &str) -> Option<String> {
    let mut search = 0usize;
    while let Some(rel) = open_tag[search..].find(attr) {
        let pos = search + rel;
        let before_ok = pos == 0
            || open_tag[..pos]
                .chars()
                .next_back()
                .map(|c| c.is_whitespace() || c == '<')
                .unwrap_or(true);
        let after = pos + attr.len();
        if !before_ok {
            search = after;
            continue;
        }
        let rest = &open_tag[after..];
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            if let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let value = &rest[quote.len_utf8()..];
                if let Some(end) = value.find(quote) {
                    return Some(value[..end].to_string());
                }
            }
        }
        search = after;
    }
    None
}

fn is_tag_boundary(text: &str, index: usize) -> bool {
    match text[index..].chars().next() {
        None => true,
        Some(c) => c == '>' || c == '/' || c.is_whitespace(),
    }
}

fn close_tag_for(open_tag: &str) -> Option<&'static str> {
    for (open, close, family) in TOOL_TAG_PAIRS {
        if open_tag == open || open_tag == family || open_tag == &open[1..] {
            return Some(close);
        }
    }
    for (open, close) in DSML_TAG_PAIRS {
        if open_tag == open || open_tag == "dsml" {
            return Some(close);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_xml_and_invoke_tags() {
        let t1 = "Here is the call: <tool_call>{\"name\": \"Bash\"}</tool_call>";
        let m1 = scan_tool_tag_start(t1).unwrap();
        assert_eq!(m1.tag_name, "tool_call");
        assert_eq!(&t1[m1.start..m1.content_start], "<tool_call>");

        let t2 =
            "Command: <invoke name=\"Bash\"><parameter name=\"command\">pwd</parameter></invoke>";
        let m2 = scan_tool_tag_start(t2).unwrap();
        assert_eq!(m2.tag_name, "invoke");
        assert_eq!(m2.name.as_deref(), Some("Bash"));
    }

    #[test]
    fn find_end_and_partial_prefix() {
        let text = "x<tool_call>{\"a\":1}</tool_call>y";
        let m = scan_tool_tag_start(text).unwrap();
        let end = scan_tool_tag_end(&text[m.content_start..], m.tag_name).unwrap();
        assert_eq!(&text[m.content_start..m.content_start + end], "{\"a\":1}");
        assert_eq!(partial_tag_prefix_len("hello <tool_"), "<tool_".len());
    }

    #[test]
    fn strips_tags_and_preserves_surrounding_text() {
        let text = "before <tool_call>{\"name\":\"Bash\"}</tool_call> after";
        assert_eq!(strip_tool_tags(text), "before  after");
    }

    #[test]
    fn streamable_end_holds_partial_tag() {
        assert_eq!(streamable_text_end("hello <tool_"), "hello ".len());
        assert_eq!(streamable_text_end("hello <tool_call>"), "hello ".len());
    }
}
