//! Incremental search over the buffer: case-insensitive substring
//! matching in char space (unicode-safe), plus the live search state.

/// One occurrence of the query: `start..end` *char* indices on `line`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Case-insensitive comparison of two chars (full Unicode lowercase
/// folding, so `É` matches `é`).
fn chars_equal(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

/// Find all non-overlapping occurrences of `query` in `lines`,
/// case-insensitively. Returns char-index ranges sorted by line and start.
/// An empty query matches nothing.
pub fn find_matches(lines: &[String], query: &str) -> Vec<Match> {
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let text: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i + query.len() <= text.len() {
            let hit = query
                .iter()
                .zip(&text[i..i + query.len()])
                .all(|(&q, &c)| chars_equal(q, c));
            if hit {
                out.push(Match {
                    line: line_idx,
                    start: i,
                    end: i + query.len(),
                });
                i += query.len(); // non-overlapping, like vim/VSCode
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Which field of a find-and-replace prompt is being edited.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchField {
    Query,
    Replacement,
}

/// Live search state: the query, its matches in the current buffer, and
/// which match is "current" (the one the cursor sits on).
pub struct Search {
    /// The query being searched for (editable while searching).
    pub query: String,
    /// Matches for `query` in the buffer, sorted by (line, start).
    matches: Vec<Match>,
    /// Index into `matches` of the current match.
    current: usize,
    /// When `Some`, this is find-and-replace rather than find-only. An
    /// empty string is a valid replacement (delete each match).
    pub replacement: Option<String>,
    /// Which prompt field is being edited. Ignored in find-only mode.
    pub field: SearchField,
}

impl Search {
    /// A new, empty find-only search.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            matches: Vec::new(),
            current: 0,
            replacement: None,
            field: SearchField::Query,
        }
    }

    /// A new find-and-replace prompt, starting on the find field.
    pub fn new_replace() -> Self {
        let mut search = Self::new();
        search.replacement = Some(String::new());
        search
    }

    /// Turn a find-only search into find-and-replace, keeping the query.
    /// If the query is already non-empty, editing moves to the replacement
    /// field so a newcomer can type find, then type the replacement.
    pub fn enable_replace(&mut self) {
        if self.replacement.is_none() {
            self.replacement = Some(String::new());
        }
        self.field = if self.query.is_empty() {
            SearchField::Query
        } else {
            SearchField::Replacement
        };
    }

    /// Whether this search is in find-and-replace mode.
    pub fn is_replace(&self) -> bool {
        self.replacement.is_some()
    }

    /// The replacement string, if this is a find-and-replace.
    pub fn replacement_text(&self) -> Option<&str> {
        self.replacement.as_deref()
    }

    /// All current matches, in document order.
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// How many matches the query currently has.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Index of the current match within the match list.
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// Recompute the matches for the current query. `cursor` is the buffer
    /// cursor `(char, line)`: the current match becomes the first match at
    /// or after it, wrapping to the first match when there is none.
    pub fn refresh(&mut self, lines: &[String], cursor: (usize, usize)) {
        self.matches = find_matches(lines, &self.query);
        self.current = self
            .matches
            .iter()
            .position(|m| (m.line, m.start) >= (cursor.1, cursor.0))
            .unwrap_or(0);
    }

    /// Move the current match by `dir` (+1 next, −1 previous), wrapping
    /// around at both ends. Does nothing when there are no matches.
    pub fn step(&mut self, dir: isize) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.current = (self.current as isize + dir).rem_euclid(n) as usize;
    }

    /// The current match, if any.
    pub fn current_match(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// `(start, end, is_current)` char ranges of the matches on line `y`.
    pub fn matches_on_line(&self, y: usize) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        let cur = self.matches.get(self.current).copied();
        self.matches
            .iter()
            .filter(move |m| m.line == y)
            .map(move |m| (m.start, m.end, Some(*m) == cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(contents: &[&str]) -> Vec<String> {
        contents.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finds_all_occurrences_across_lines() {
        let lines = texts(&["foo bar foo", "nothing", "foo"]);
        assert_eq!(
            find_matches(&lines, "foo"),
            vec![
                Match {
                    line: 0,
                    start: 0,
                    end: 3
                },
                Match {
                    line: 0,
                    start: 8,
                    end: 11
                },
                Match {
                    line: 2,
                    start: 0,
                    end: 3
                },
            ]
        );
    }

    #[test]
    fn case_insensitive() {
        let lines = texts(&["Hello HELLO hElLo"]);
        assert_eq!(find_matches(&lines, "hello").len(), 3);
        assert_eq!(find_matches(&lines, "HELLO").len(), 3);
    }

    #[test]
    fn unicode_case_folding_and_wide_chars() {
        // É folds to é; 本 is a wide char but char indices count chars
        let lines = texts(&["héllo wörld"]);
        assert_eq!(
            find_matches(&lines, "ÉLL"),
            vec![Match {
                line: 0,
                start: 1,
                end: 4
            }]
        );
        let lines = texts(&["日本語テキスト"]);
        assert_eq!(
            find_matches(&lines, "本"),
            vec![Match {
                line: 0,
                start: 1,
                end: 2
            }]
        );
    }

    #[test]
    fn matches_do_not_overlap() {
        let lines = texts(&["aaaa"]);
        assert_eq!(find_matches(&lines, "aa").len(), 2);
        let lines = texts(&["aaa"]);
        assert_eq!(find_matches(&lines, "aa").len(), 1);
    }

    #[test]
    fn empty_query_and_no_hits_match_nothing() {
        let lines = texts(&["hello world"]);
        assert!(find_matches(&lines, "").is_empty());
        assert!(find_matches(&lines, "zzz").is_empty());
    }

    #[test]
    fn refresh_anchors_current_match_at_the_cursor() {
        let lines = texts(&["aa", "aa", "aa"]);
        let mut s = Search::new();
        s.query = "aa".to_string();
        s.refresh(&lines, (0, 1)); // cursor on line 1
        assert_eq!(s.current_index(), 1);
        s.refresh(&lines, (5, 1)); // past the match on line 1 -> line 2
        assert_eq!(s.current_index(), 2);
        s.refresh(&lines, (0, 2)); // at the last match
        assert_eq!(s.current_index(), 2);
        s.refresh(&lines, (9, 9)); // past everything -> wraps to the first
        assert_eq!(s.current_index(), 0);
        assert_eq!(
            s.current_match(),
            Some(Match {
                line: 0,
                start: 0,
                end: 2
            })
        );
    }

    #[test]
    fn empty_query_has_no_current_match() {
        let lines = texts(&["aa"]);
        let mut s = Search::new();
        s.query = "aa".to_string();
        s.refresh(&lines, (0, 0));
        assert_eq!(s.match_count(), 1);
        s.query.clear();
        s.refresh(&lines, (0, 0));
        assert_eq!(s.match_count(), 0);
        assert_eq!(s.current_match(), None);
        s.step(1); // stepping with no matches is a no-op
        assert_eq!(s.current_index(), 0);
    }

    #[test]
    fn step_wraps_in_both_directions() {
        let lines = texts(&["aa aa aa"]);
        let mut s = Search::new();
        s.query = "aa".to_string();
        s.refresh(&lines, (0, 0));
        assert_eq!(s.match_count(), 3);
        s.step(-1); // previous from the first wraps to the last
        assert_eq!(s.current_index(), 2);
        s.step(1); // next from the last wraps to the first
        assert_eq!(s.current_index(), 0);
        s.step(5);
        assert_eq!(s.current_index(), 2);
    }

    #[test]
    fn matches_on_line_flags_the_current_one() {
        let lines = texts(&["aa", "aa"]);
        let mut s = Search::new();
        s.query = "aa".to_string();
        s.refresh(&lines, (0, 0));
        assert_eq!(s.matches_on_line(0).collect::<Vec<_>>(), vec![(0, 2, true)]);
        assert_eq!(
            s.matches_on_line(1).collect::<Vec<_>>(),
            vec![(0, 2, false)]
        );
        assert_eq!(
            s.matches_on_line(9).collect::<Vec<_>>(),
            Vec::<(usize, usize, bool)>::new()
        );
    }

    #[test]
    fn enable_replace_keeps_the_query_and_moves_to_the_replacement_field() {
        let mut s = Search::new();
        s.query = "foo".to_string();
        s.enable_replace();
        assert!(s.is_replace());
        assert_eq!(s.query, "foo");
        assert_eq!(s.replacement_text(), Some(""));
        assert_eq!(s.field, SearchField::Replacement);

        let mut empty = Search::new_replace();
        assert!(empty.is_replace());
        assert_eq!(empty.field, SearchField::Query);
        empty.enable_replace();
        assert_eq!(empty.field, SearchField::Query);
    }
}
