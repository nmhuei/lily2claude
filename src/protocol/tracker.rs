//! SseBlockTracker — Manages content block indices and state transitions for Anthropic SSE.
//!
//! Enforces the rule that at most one thinking block and one text block can be open at a time,
//! ensuring correct ordering and allocation of block indices.

#[derive(Debug, Default, Clone)]
pub struct SseBlockTracker {
    thinking_idx: Option<usize>,
    text_idx: Option<usize>,
    next_idx: usize,
    ever_opened: bool,
}

impl SseBlockTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return true if any block has ever been opened since creation.
    pub fn has_any_blocks_ever_opened(&self) -> bool {
        self.ever_opened
    }

    /// Allocate and return the next content block index.
    pub fn next_index(&mut self) -> usize {
        let i = self.next_idx;
        self.next_idx += 1;
        self.ever_opened = true;
        i
    }

    /// Returns the current thinking block index, if open.
    pub fn thinking_idx(&self) -> Option<usize> {
        self.thinking_idx
    }

    /// Returns the current text block index, if open.
    pub fn text_idx(&self) -> Option<usize> {
        self.text_idx
    }

    /// Returns true if any thinking or text block is open.
    pub fn has_open_blocks(&self) -> bool {
        self.thinking_idx.is_some() || self.text_idx.is_some()
    }

    /// Ensure a thinking block is open, creating one if needed.
    /// Returns `(thinking_index, is_new, optional_closed_text_index)`.
    pub fn ensure_thinking(&mut self) -> (usize, bool, Option<usize>) {
        if let Some(idx) = self.thinking_idx {
            return (idx, false, None);
        }
        let closed_text = self.text_idx.take();
        let idx = self.next_index();
        self.thinking_idx = Some(idx);
        (idx, true, closed_text)
    }

    /// Ensure a text block is open, creating one if needed.
    /// Returns `(text_index, is_new, optional_closed_thinking_index)`.
    pub fn ensure_text(&mut self) -> (usize, bool, Option<usize>) {
        if let Some(idx) = self.text_idx {
            return (idx, false, None);
        }
        let closed_thinking = self.thinking_idx.take();
        let idx = self.next_index();
        self.text_idx = Some(idx);
        (idx, true, closed_thinking)
    }

    /// Close the thinking block, returning its index if it was open.
    pub fn close_thinking(&mut self) -> Option<usize> {
        self.thinking_idx.take()
    }

    /// Close the text block, returning its index if it was open.
    pub fn close_text(&mut self) -> Option<usize> {
        self.text_idx.take()
    }

    /// Close all open blocks, returning a list of `("type", block_index)` pairs.
    pub fn close_all(&mut self) -> Vec<(&'static str, usize)> {
        let mut closed = Vec::new();
        if let Some(idx) = self.thinking_idx.take() {
            closed.push(("thinking", idx));
        }
        if let Some(idx) = self.text_idx.take() {
            closed.push(("text", idx));
        }
        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracker_lifecycle() {
        let mut tracker = SseBlockTracker::new();
        assert!(!tracker.has_open_blocks());

        // 1. Open thinking
        let (t_idx, is_new, closed) = tracker.ensure_thinking();
        assert_eq!(t_idx, 0);
        assert!(is_new);
        assert_eq!(closed, None);
        assert_eq!(tracker.thinking_idx(), Some(0));
        assert!(tracker.has_open_blocks());

        // 2. Re-ensure thinking does not re-open
        let (t_idx2, is_new2, closed2) = tracker.ensure_thinking();
        assert_eq!(t_idx2, 0);
        assert!(!is_new2);
        assert_eq!(closed2, None);

        // 3. Switch to text: should auto-close thinking
        let (txt_idx, is_new3, closed3) = tracker.ensure_text();
        assert_eq!(txt_idx, 1);
        assert!(is_new3);
        assert_eq!(closed3, Some(0)); // closed thinking at index 0
        assert_eq!(tracker.thinking_idx(), None);
        assert_eq!(tracker.text_idx(), Some(1));

        // 4. Switch back to thinking: should auto-close text
        let (t_idx3, is_new4, closed4) = tracker.ensure_thinking();
        assert_eq!(t_idx3, 2);
        assert!(is_new4);
        assert_eq!(closed4, Some(1)); // closed text at index 1
        assert_eq!(tracker.thinking_idx(), Some(2));
        assert_eq!(tracker.text_idx(), None);

        // 5. Close all
        let closed_all = tracker.close_all();
        assert_eq!(closed_all, vec![("thinking", 2)]);
        assert!(!tracker.has_open_blocks());
    }
}
