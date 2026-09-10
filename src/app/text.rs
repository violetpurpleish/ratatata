//! Unicode-aware clipping and styled text helpers used by the editor.

use super::*;

/// Shorten `s` to at most `max` chars, keeping the end and prefixing "…".
pub(super) fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let keep = max - 1;
    let mut out: String = s.chars().skip(count - keep).collect();
    out.insert(0, '…');
    out
}

/// Byte index of the `idx`-th char in `s` (or `s.len()` if past the end).
pub(super) fn char_index_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Char index at (or just past) terminal column `col`, accounting for wide
/// characters: the cursor lands between chars at the clicked cell boundary.
pub(super) fn char_at_col(s: &str, col: usize) -> usize {
    let mut width = 0;
    for (i, c) in s.chars().enumerate() {
        if width >= col {
            return i;
        }
        width += char_width(c);
    }
    s.chars().count()
}

/// Snap a byte offset up to the next char boundary.
pub(super) fn snap_char_up(s: &str, mut b: usize) -> usize {
    while b < s.len() && !s.is_char_boundary(b) {
        b += 1;
    }
    b
}

/// Snap a byte offset down to the previous char boundary.
pub(super) fn snap_char_down(s: &str, mut b: usize) -> usize {
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Clip styled ranges from the highlighter to the visible slice starting at
/// `start_char` and occupying at most `max_columns` display columns (tabs
/// expand to the editor tab width). Interpreting this as a char count let
/// unwrapped long lines (and tab-expanded text) paint over the editor's
/// right border.
///
/// `None` styles render as plain text; spans overlapping `sel` (a byte range
/// on this line) get the theme's selection background, and spans inside a
/// search match (`matches`, char ranges with a "current match" flag) get the
/// match background. On limited-color terminals, RGB styles are mapped; the
/// truecolor path leaves them unchanged.
pub(super) fn clip_ops<'a>(
    line: &'a str,
    ops: &[(Option<Style>, Range<usize>)],
    start_char: usize,
    max_columns: usize,
    sel: Option<(usize, usize)>,
    matches: &[(usize, usize, bool)],
    color_support: ColorSupport,
) -> Vec<Span<'a>> {
    let pal = theme::ui_palette(color_support, PALETTE);
    let search_current = pal.warning;
    let search_other = pal.selection;
    let mut cols = 0;
    let mut end_char = start_char;
    for c in line.chars().skip(start_char) {
        let cw = char_width(c);
        if cols + cw > max_columns {
            break;
        }
        cols += cw;
        end_char += 1;
    }
    let start_byte = char_index_to_byte(line, start_char);
    let end_byte = char_index_to_byte(line, end_char);
    let matches: Vec<(usize, usize, bool)> = matches
        .iter()
        .map(|&(a, b, current)| {
            (
                char_index_to_byte(line, a),
                char_index_to_byte(line, b),
                current,
            )
        })
        .collect();
    let mut out = Vec::new();
    for (style, range) in ops {
        let a = range.start.max(start_byte);
        let b = range.end.min(end_byte);
        if a >= b {
            continue;
        }
        // syntect ranges are char-aligned, but be safe
        let a = snap_char_up(line, a);
        let b = snap_char_down(line, b);
        if a >= b {
            continue;
        }
        // split the range at every selection and match boundary so each
        // piece can carry its own style
        let mut cuts = vec![a, b];
        if let Some((sa, sb)) = sel {
            cuts.extend([sa, sb]);
        }
        for &(ma, mb, _) in &matches {
            cuts.extend([ma, mb]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let (ca, cb) = (pair[0], pair[1]);
            if ca < a || cb > b || ca >= cb {
                continue;
            }
            let mut style = theme::adapt_style(color_support, style.unwrap_or_default());
            if let Some(&(_, _, current)) =
                matches.iter().find(|&&(ma, mb, _)| ca >= ma && cb <= mb)
            {
                let bg = if current {
                    search_current
                } else {
                    search_other
                };
                style = theme::highlight_style(color_support, style, bg);
            }
            if sel.is_some_and(|(sa, sb)| ca >= sa && cb <= sb) {
                if style.fg.is_none() {
                    style = style.fg(pal.fg);
                }
                style = theme::highlight_style(color_support, style, pal.selection);
            }
            let text = expand_tabs(&line[ca..cb]);
            if style == Style::default() {
                out.push(Span::raw(text));
            } else {
                out.push(Span::styled(text, style));
            }
        }
    }
    out
}
