enum Segment {
    Typed(Vec<char>),
    Pasted(String),
}

impl Segment {
    fn display_len(&self) -> usize {
        match self {
            Segment::Typed(chars) => chars.len(),
            Segment::Pasted(content) => paste_summary(content).len(),
        }
    }
}

fn paste_summary(content: &str) -> String {
    // Normalize \r\n and bare \r to \n, then count lines
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let n = normalized.lines().count().max(1);
    format!("[{n} lines pasted]")
}

pub struct InputState {
    /// Alternating sequence: always starts and ends with `Typed`, with
    /// `Pasted` segments interleaved: `[T, (P, T)*]`.
    segments: Vec<Segment>,
    /// Index into `segments` — always points to a `Typed` variant.
    cursor_seg: usize,
    /// Character offset within the current `Typed` segment.
    cursor_off: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            segments: vec![Segment::Typed(Vec::new())],
            cursor_seg: 0,
            cursor_off: 0,
        }
    }

    /// Returns the full content for submission, expanding pasted blocks.
    pub fn buffer(&self) -> String {
        self.segments
            .iter()
            .map(|seg| match seg {
                Segment::Typed(chars) => chars.iter().collect::<String>(),
                Segment::Pasted(content) => content.clone(),
            })
            .collect()
    }

    /// Returns the display representation: typed text shown verbatim,
    /// paste blocks shown as `[N lines pasted]` summaries.
    pub fn display_text(&self) -> String {
        self.segments
            .iter()
            .map(|seg| match seg {
                Segment::Typed(chars) => chars.iter().collect::<String>(),
                Segment::Pasted(content) => paste_summary(content),
            })
            .collect()
    }

    /// Cursor position within the display string returned by [`display_text`].
    pub fn display_cursor(&self) -> usize {
        let mut pos = 0;
        for (i, seg) in self.segments.iter().enumerate() {
            if i == self.cursor_seg {
                return pos + self.cursor_off;
            }
            pos += seg.display_len();
        }
        pos + self.cursor_off
    }

    /// Returns the buffer content as a lowercase char vector, suitable for fuzzy matching.
    pub fn char_vec(&self) -> Vec<char> {
        self.buffer().to_lowercase().chars().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|seg| match seg {
            Segment::Typed(chars) => chars.is_empty(),
            Segment::Pasted(_) => false,
        })
    }

    #[cfg(test)]
    fn has_paste(&self) -> bool {
        self.segments
            .iter()
            .any(|seg| matches!(seg, Segment::Pasted(_)))
    }

    /// Accept pasted text: multi-line pastes become a `Pasted` block,
    /// single-line pastes are inserted character-by-character.
    pub fn accept_paste(&mut self, text: String) {
        if text.contains('\n') || text.contains('\r') {
            self.set_paste(text);
        } else {
            for c in text.chars() {
                self.insert(c);
            }
        }
    }

    /// Insert a multi-line paste at the current cursor position.
    /// Splits the current `Typed` segment and inserts a `Pasted` block between
    /// the two halves.  The cursor moves to the start of the second half so
    /// subsequent typing appears *after* the paste summary.
    pub fn set_paste(&mut self, text: String) {
        // Normalize line endings: \r\n → \n, bare \r → \n
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (before, after) = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => (
                chars[..self.cursor_off].to_vec(),
                chars[self.cursor_off..].to_vec(),
            ),
            Segment::Pasted(_) => (vec![], vec![]),
        };
        self.segments[self.cursor_seg] = Segment::Typed(before);
        let paste_idx = self.cursor_seg + 1;
        self.segments.insert(paste_idx, Segment::Pasted(text));
        self.segments.insert(paste_idx + 1, Segment::Typed(after));
        self.cursor_seg = paste_idx + 1;
        self.cursor_off = 0;
    }

    pub fn insert(&mut self, c: char) {
        if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
            chars.insert(self.cursor_off, c);
            self.cursor_off += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_off > 0 {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                self.cursor_off -= 1;
                chars.remove(self.cursor_off);
            }
        } else if self.cursor_seg >= 2 {
            // At start of Typed preceded by Pasted — delete the paste block.
            let prev_typed_idx = self.cursor_seg - 2;
            let prev_len = match &self.segments[prev_typed_idx] {
                Segment::Typed(chars) => chars.len(),
                _ => 0,
            };
            let current_chars = match &self.segments[self.cursor_seg] {
                Segment::Typed(chars) => chars.clone(),
                _ => vec![],
            };
            self.segments.remove(self.cursor_seg);
            self.segments.remove(self.cursor_seg - 1);
            if let Segment::Typed(ref mut chars) = self.segments[prev_typed_idx] {
                chars.extend(current_chars);
            }
            self.cursor_seg = prev_typed_idx;
            self.cursor_off = prev_len;
        }
    }

    pub fn delete(&mut self) {
        let at_end = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => self.cursor_off >= chars.len(),
            _ => true,
        };
        if !at_end {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                chars.remove(self.cursor_off);
            }
        } else if self.cursor_seg + 2 < self.segments.len() {
            // At end of Typed followed by Pasted — delete the paste block.
            let next_typed_chars = match &self.segments[self.cursor_seg + 2] {
                Segment::Typed(chars) => chars.clone(),
                _ => vec![],
            };
            self.segments.remove(self.cursor_seg + 2);
            self.segments.remove(self.cursor_seg + 1);
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                chars.extend(next_typed_chars);
            }
        }
    }

    pub fn left(&mut self) {
        if self.cursor_off > 0 {
            self.cursor_off -= 1;
        } else if self.cursor_seg >= 2 {
            self.cursor_seg -= 2;
            self.cursor_off = match &self.segments[self.cursor_seg] {
                Segment::Typed(chars) => chars.len(),
                _ => 0,
            };
        }
    }

    pub fn right(&mut self) {
        let at_end = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => self.cursor_off >= chars.len(),
            _ => true,
        };
        if !at_end {
            self.cursor_off += 1;
        } else if self.cursor_seg + 2 < self.segments.len() {
            self.cursor_seg += 2;
            self.cursor_off = 0;
        }
    }

    pub fn home(&mut self) {
        self.cursor_seg = 0;
        self.cursor_off = 0;
    }

    pub fn end(&mut self) {
        self.cursor_seg = self.segments.len() - 1;
        self.cursor_off = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => chars.len(),
            _ => 0,
        };
    }

    pub fn kill_line(&mut self) {
        if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
            chars.truncate(self.cursor_off);
        }
        self.segments.truncate(self.cursor_seg + 1);
    }

    pub fn kill_before(&mut self) {
        let remaining = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => chars[self.cursor_off..].to_vec(),
            _ => vec![],
        };
        let after: Vec<Segment> = self.segments.drain(self.cursor_seg + 1..).collect();
        self.segments.clear();
        self.segments.push(Segment::Typed(remaining));
        self.segments.extend(after);
        self.cursor_seg = 0;
        self.cursor_off = 0;
    }

    pub fn kill_word(&mut self) {
        if self.cursor_off > 0 {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                let mut i = self.cursor_off;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                chars.drain(i..self.cursor_off);
                self.cursor_off = i;
            }
        } else if self.cursor_seg >= 2 {
            // Delete preceding paste block (treat it as one "word").
            self.backspace();
        }
    }

    pub fn take(&mut self) -> String {
        let result = self.buffer();
        self.segments = vec![Segment::Typed(Vec::new())];
        self.cursor_seg = 0;
        self.cursor_off = 0;
        result
    }

    pub fn set(&mut self, text: &str) {
        self.segments = vec![Segment::Typed(text.chars().collect())];
        self.cursor_seg = 0;
        self.cursor_off = text.chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── basic typing ──────────────────────────────────────────────

    #[test]
    fn single_char_insert() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.display_cursor(), 3);
    }

    #[test]
    fn insert_at_cursor() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('c');
        input.left();
        input.insert('b');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.display_cursor(), 2);
    }

    #[test]
    fn single_line_paste_chars_then_type() {
        // Simulates single-line paste (inserted char-by-char in event loop)
        let mut input = InputState::new();
        for c in "pasted".chars() {
            input.insert(c);
        }
        assert_eq!(input.buffer(), "pasted");
        input.insert('!');
        assert_eq!(input.buffer(), "pasted!");
    }

    // ── paste inserts at cursor, doesn't replace ──────────────────

    #[test]
    fn paste_inserts_at_cursor_position() {
        let mut input = InputState::new();
        for c in "hello world".chars() {
            input.insert(c);
        }
        // Move cursor back to after "hello "
        for _ in 0..5 {
            input.left();
        }
        input.set_paste("fn foo() {\n    bar();\n}".to_string());
        // Typed text before and after paste is preserved
        assert_eq!(input.buffer(), "hello fn foo() {\n    bar();\n}world");
        assert_eq!(input.display_text(), "hello [3 lines pasted]world");
    }

    #[test]
    fn paste_at_beginning_preserves_typed() {
        let mut input = InputState::new();
        for c in "suffix".chars() {
            input.insert(c);
        }
        input.home();
        input.set_paste("a\nb".to_string());
        assert_eq!(input.buffer(), "a\nbsuffix");
        assert_eq!(input.display_text(), "[2 lines pasted]suffix");
    }

    #[test]
    fn paste_at_end_preserves_typed() {
        let mut input = InputState::new();
        for c in "prefix ".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        assert_eq!(input.buffer(), "prefix a\nb");
        assert_eq!(input.display_text(), "prefix [2 lines pasted]");
    }

    // ── typing after paste keeps summary stable ───────────────────

    #[test]
    fn typing_after_paste_keeps_summary() {
        let mut input = InputState::new();
        input.set_paste("line1\nline2\nline3".to_string());
        assert!(input.has_paste());

        // Typing appends *after* the paste summary — summary stays
        input.insert('!');
        assert!(input.has_paste());
        assert_eq!(input.buffer(), "line1\nline2\nline3!");
        assert_eq!(input.display_text(), "[3 lines pasted]!");
    }

    #[test]
    fn multiple_inserts_after_paste() {
        let mut input = InputState::new();
        input.set_paste("base\nline".to_string());
        input.insert('!');
        input.insert('!');
        input.insert('!');
        assert_eq!(input.buffer(), "base\nline!!!");
        assert_eq!(input.display_text(), "[2 lines pasted]!!!");
    }

    // ── backspace / delete at paste boundary ──────────────────────

    #[test]
    fn backspace_at_paste_boundary_removes_paste_block() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor is right after paste; backspace deletes the whole block
        input.backspace();
        assert!(!input.has_paste());
        assert!(input.is_empty());
    }

    #[test]
    fn backspace_after_paste_with_typed_preserves_typed() {
        let mut input = InputState::new();
        for c in "before ".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        // Backspace deletes the paste block, not "before "
        input.backspace();
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "before ");
    }

    #[test]
    fn delete_at_paste_boundary_removes_paste_block() {
        let mut input = InputState::new();
        for c in "before".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        for c in " after".chars() {
            input.insert(c);
        }
        // Move cursor to end of "before" (just before paste)
        input.home();
        input.right(); // b
        input.right(); // e
        input.right(); // f
        input.right(); // o
        input.right(); // r
        input.right(); // e — end of first Typed, still in seg 0
        // Now delete should remove the paste block
        input.delete();
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "before after");
    }

    #[test]
    fn backspace_on_empty_after_paste_clear() {
        let mut input = InputState::new();
        input.set_paste("x\ny".to_string());
        input.backspace(); // removes the paste block
        assert!(input.is_empty());
        // Another backspace is safe
        input.backspace();
        assert!(input.is_empty());
    }

    #[test]
    fn paste_then_delete_at_end_is_noop() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor after paste, no following content — delete is noop
        input.delete();
        assert_eq!(input.buffer(), "hello\nworld");
    }

    // ── cursor movement skips paste blocks ────────────────────────

    #[test]
    fn cursor_skips_paste_on_left_right() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Display: "ab[2 lines pasted]cd"
        let summary_len = "[2 lines pasted]".len();
        assert_eq!(input.display_cursor(), 2 + summary_len + 2); // end

        // Move left twice into "cd"
        input.left();
        assert_eq!(input.display_cursor(), 2 + summary_len + 1);
        input.left();
        assert_eq!(input.display_cursor(), 2 + summary_len);

        // Next left skips the paste block → end of "ab"
        input.left();
        assert_eq!(input.display_cursor(), 2);

        // Right skips paste → start of "cd"
        input.right();
        assert_eq!(input.display_cursor(), 2 + summary_len);
    }

    #[test]
    fn home_and_end_span_all_segments() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        let total = input.display_text().len();

        input.home();
        assert_eq!(input.display_cursor(), 0);
        input.end();
        assert_eq!(input.display_cursor(), total);
    }

    // ── kill operations ───────────────────────────────────────────

    #[test]
    fn kill_line_from_before_paste() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Move home then right once → cursor at 'b'
        input.home();
        input.right();
        // Kill line: keep "a", remove rest including paste
        input.kill_line();
        assert_eq!(input.buffer(), "a");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_line_from_end_is_noop() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor at end (after paste, in empty Typed)
        input.kill_line();
        assert_eq!(input.buffer(), "hello\nworld");
    }

    #[test]
    fn kill_before_from_after_paste() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Cursor at end of "cd". Kill before removes "ab" + paste + "cd"
        input.kill_before();
        assert_eq!(input.buffer(), "");
        assert_eq!(input.display_cursor(), 0);
    }

    #[test]
    fn kill_before_preserves_segments_after_cursor() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Move to start of "cd" (just after paste)
        input.left();
        input.left();
        // Kill before: removes "ab" and paste, keeps "cd"
        input.kill_before();
        assert_eq!(input.buffer(), "cd");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_word_at_paste_boundary() {
        let mut input = InputState::new();
        for c in "hello ".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        // Cursor right after paste, in empty Typed. kill_word deletes paste.
        input.kill_word();
        assert_eq!(input.buffer(), "hello ");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_word_within_typed_after_paste() {
        let mut input = InputState::new();
        input.set_paste("x\ny".to_string());
        for c in "hello world".chars() {
            input.insert(c);
        }
        // Cursor at end of "hello world" typed segment
        input.kill_word();
        assert_eq!(input.buffer(), "x\nyhello ");
        assert_eq!(input.display_text(), "[2 lines pasted]hello ");
    }

    // ── take / set / buffer ───────────────────────────────────────

    #[test]
    fn take_returns_expanded_content() {
        let mut input = InputState::new();
        for c in "before ".chars() {
            input.insert(c);
        }
        input.set_paste("pasted\ntext".to_string());
        for c in " after".chars() {
            input.insert(c);
        }
        let taken = input.take();
        assert_eq!(taken, "before pasted\ntext after");
        assert!(input.is_empty());
    }

    #[test]
    fn paste_take_returns_pasted_content() {
        let mut input = InputState::new();
        input.set_paste("pasted\ntext".to_string());
        let taken = input.take();
        assert_eq!(taken, "pasted\ntext");
        assert!(input.is_empty());
    }

    #[test]
    fn paste_buffer_returns_pasted_content() {
        let mut input = InputState::new();
        input.set_paste("multi\nline".to_string());
        assert_eq!(input.buffer(), "multi\nline");
    }

    #[test]
    fn set_after_paste_replaces() {
        let mut input = InputState::new();
        input.set_paste("pasted\nstuff".to_string());
        input.set("replaced");
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "replaced");
    }

    // ── multiple pastes ───────────────────────────────────────────

    #[test]
    fn multiple_pastes_both_shown_as_summaries() {
        let mut input = InputState::new();
        input.set_paste("a\nb".to_string());
        for c in " ".chars() {
            input.insert(c);
        }
        input.set_paste("c\nd".to_string());
        assert_eq!(input.display_text(), "[2 lines pasted] [2 lines pasted]");
        assert_eq!(input.buffer(), "a\nb c\nd");
    }

    #[test]
    fn backspace_removes_only_adjacent_paste() {
        let mut input = InputState::new();
        input.set_paste("a\nb".to_string());
        for c in " ".chars() {
            input.insert(c);
        }
        input.set_paste("c\nd".to_string());
        // Backspace removes second paste only
        input.backspace();
        assert!(input.has_paste()); // first paste still there
        assert_eq!(input.buffer(), "a\nb ");
        assert_eq!(input.display_text(), "[2 lines pasted] ");
    }
}
