//! ThinkingRouter — Intercepts leaked thinking tags, segments long reasoning,
//! and paces token deltas for smooth Anthropic SSE streaming.

use crate::protocol::sanitize::{
    find_any_tag, split_utf8_prefix, THINK_CLOSE_TAGS, THINK_OPEN_TAGS,
};

pub const THINKING_RENDER_CHUNK_BYTES: usize = 16384;
pub const RENDER_DELTA_CHUNK_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ThinkingRouteItem {
    Thinking(String),
    Text(String),
    SegmentBreak,
}

#[derive(Debug, Default)]
pub struct ThinkingRouter {
    pub in_think_tag: bool,
    pub thinking_block_bytes: usize,
    pub pending_buffer: String,
}

impl ThinkingRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a dedicated reasoning chunk (e.g. from ACP agent_thought_chunk).
    pub fn process_thinking(&mut self, chunk: &str) -> Vec<ThinkingRouteItem> {
        let mut items = Vec::new();
        if chunk.is_empty() {
            return items;
        }

        let mut remaining = chunk;
        while !remaining.is_empty() {
            let room = THINKING_RENDER_CHUNK_BYTES.saturating_sub(self.thinking_block_bytes);
            if room == 0 {
                items.push(ThinkingRouteItem::SegmentBreak);
                self.thinking_block_bytes = 0;
                continue;
            }

            let (part, rest) = split_utf8_prefix(remaining, room);
            if !part.is_empty() {
                items.push(ThinkingRouteItem::Thinking(part.to_string()));
                self.thinking_block_bytes += part.len();
            }
            remaining = rest;

            if self.thinking_block_bytes >= THINKING_RENDER_CHUNK_BYTES {
                items.push(ThinkingRouteItem::SegmentBreak);
                self.thinking_block_bytes = 0;
            }
        }

        items
    }

    /// Process a text chunk which might contain embedded or leaked `<think>` / `</think>` tags.
    pub fn process_text(&mut self, chunk: &str) -> Vec<ThinkingRouteItem> {
        let mut items = Vec::new();
        self.pending_buffer.push_str(chunk);

        loop {
            if self.pending_buffer.is_empty() {
                break;
            }

            if self.in_think_tag {
                // Currently inside thinking mode: look for close tag
                if let Some((close_pos, close_len)) = find_any_tag(&self.pending_buffer, &THINK_CLOSE_TAGS) {
                    let thought = self.pending_buffer[..close_pos].to_string();
                    self.pending_buffer.drain(..close_pos + close_len);
                    self.in_think_tag = false;

                    if !thought.is_empty() {
                        items.extend(self.process_thinking(&thought));
                    }
                    continue;
                }

                // Close tag not found: check if buffer ends with a partial close tag prefix
                let holdback = Self::partial_tag_suffix_len(&self.pending_buffer, &THINK_CLOSE_TAGS);
                let safe_len = self.pending_buffer.len().saturating_sub(holdback);

                if safe_len > 0 {
                    let safe_thought = self.pending_buffer[..safe_len].to_string();
                    self.pending_buffer.drain(..safe_len);
                    items.extend(self.process_thinking(&safe_thought));
                }
                break;
            } else {
                // Normal text mode: check for open tag OR leaked close tag
                let open_info = find_any_tag(&self.pending_buffer, &THINK_OPEN_TAGS);
                let close_info = find_any_tag(&self.pending_buffer, &THINK_CLOSE_TAGS);

                match (open_info, close_info) {
                    // Close tag appears with no open tag -> leaked thinking!
                    (None, Some((c_pos, c_len))) => {
                        let leaked_thought = self.pending_buffer[..c_pos].to_string();
                        self.pending_buffer.drain(..c_pos + c_len);
                        self.in_think_tag = false;

                        if !leaked_thought.is_empty() {
                            items.extend(self.process_thinking(&leaked_thought));
                        }
                        continue;
                    }

                    // Both open and close tags present, but close tag appears first
                    (Some((o_pos, _)), Some((c_pos, c_len))) if c_pos < o_pos => {
                        let leaked_thought = self.pending_buffer[..c_pos].to_string();
                        self.pending_buffer.drain(..c_pos + c_len);
                        self.in_think_tag = false;

                        if !leaked_thought.is_empty() {
                            items.extend(self.process_thinking(&leaked_thought));
                        }
                        continue;
                    }

                    // Open tag appears first
                    (Some((o_pos, o_len)), _) => {
                        let pre_text = self.pending_buffer[..o_pos].to_string();
                        self.pending_buffer.drain(..o_pos + o_len);
                        self.in_think_tag = true;

                        if !pre_text.is_empty() {
                            items.push(ThinkingRouteItem::Text(pre_text));
                        }
                        continue;
                    }

                    // Neither open nor close tag found
                    (None, None) => {
                        let hold_open = Self::partial_tag_suffix_len(&self.pending_buffer, &THINK_OPEN_TAGS);
                        let hold_close = Self::partial_tag_suffix_len(&self.pending_buffer, &THINK_CLOSE_TAGS);
                        let holdback = hold_open.max(hold_close);
                        let safe_len = self.pending_buffer.len().saturating_sub(holdback);

                        if safe_len > 0 {
                            let text = self.pending_buffer[..safe_len].to_string();
                            self.pending_buffer.drain(..safe_len);
                            items.push(ThinkingRouteItem::Text(text));
                        }
                        break;
                    }
                }
            }
        }

        items
    }

    /// Flush any remaining buffered characters at the end of the stream.
    pub fn finish(&mut self) -> Vec<ThinkingRouteItem> {
        let mut items = Vec::new();
        if !self.pending_buffer.is_empty() {
            let leftover = std::mem::take(&mut self.pending_buffer);
            if self.in_think_tag {
                items.extend(self.process_thinking(&leftover));
            } else {
                items.push(ThinkingRouteItem::Text(leftover));
            }
        }
        items
    }

    /// Calculates the length of the longest suffix in `text` that matches
    /// any strict non-empty prefix of one of the `tags`.
    fn partial_tag_suffix_len(text: &str, tags: &[&str]) -> usize {
        let mut longest = 0;
        for &tag in tags {
            for prefix_len in (1..tag.len()).rev() {
                if !tag.is_char_boundary(prefix_len) {
                    continue;
                }
                let prefix = &tag[..prefix_len];
                if text.ends_with(prefix) {
                    longest = longest.max(prefix.len());
                    break;
                }
            }
        }
        longest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leaked_thinking_closed_by_tag() {
        let mut router = ThinkingRouter::new();
        let items = router.process_text("I need to inspect the README file.</think>Here is what I found:");
        
        assert_eq!(
            items,
            vec![
                ThinkingRouteItem::Thinking("I need to inspect the README file.".to_string()),
                ThinkingRouteItem::Text("Here is what I found:".to_string()),
            ]
        );
    }

    #[test]
    fn test_explicit_think_tags() {
        let mut router = ThinkingRouter::new();
        let items = router.process_text("Hello <think>internal reasoning</think> world!");
        
        assert_eq!(
            items,
            vec![
                ThinkingRouteItem::Text("Hello ".to_string()),
                ThinkingRouteItem::Thinking("internal reasoning".to_string()),
                ThinkingRouteItem::Text(" world!".to_string()),
            ]
        );
    }

    #[test]
    fn test_tag_split_across_streaming_chunks() {
        let mut router = ThinkingRouter::new();
        let items1 = router.process_text("<think>Deep reasoning </th");
        assert_eq!(items1, vec![ThinkingRouteItem::Thinking("Deep reasoning ".to_string())]);

        let items2 = router.process_text("ink>Final response");
        assert_eq!(items2, vec![ThinkingRouteItem::Text("Final response".to_string())]);
    }

    #[test]
    fn test_thinking_segmentation_at_16kb() {
        let mut router = ThinkingRouter::new();
        let big_chunk = "a".repeat(17000);
        let items = router.process_thinking(&big_chunk);

        // Should contain a SegmentBreak after 16KB
        let has_break = items.iter().any(|item| matches!(item, ThinkingRouteItem::SegmentBreak));
        assert!(has_break);
    }

    #[test]
    fn test_close_tag_alone() {
        let mut router = ThinkingRouter::new();
        router.in_think_tag = true;
        let items = router.process_text("</think>");
        assert_eq!(items, Vec::<ThinkingRouteItem>::new());
        assert!(!router.in_think_tag);
    }
}
