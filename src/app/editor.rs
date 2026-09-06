//! Text and image pane rendering, caret placement, and scrollbars.

use super::*;

impl App {
    pub(super) fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let title = self
            .image
            .as_ref()
            .map(|preview| preview.path.display().to_string())
            .or_else(|| self.buffer.path.as_ref().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "untitled".to_string());
        let title = truncate(&title, area.width as usize);

        let block = self.pane_block(title, PALETTE.success, self.focus == Focus::Editor);

        // An image preview replaces the text: render the block and the image
        // fitted into the inner area (the image keeps its aspect ratio and
        // is never upscaled).
        if let Some(preview) = &mut self.image {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            preview.draw(frame, inner);
            return;
        }

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let text_w = inner.width.saturating_sub(gutter_w as u16) as usize;
        let text_h = inner.height as usize;
        let viewport = (text_w as u16, text_h as u16);
        let viewport_changed = self.editor_text != viewport;
        self.editor_text = viewport;
        self.buffer.wrap_width = text_w;
        // Clamp a viewport that may have become too far down after an edit,
        // but do not otherwise reanchor it to the cursor. This distinction is
        // what lets the mouse wheel scroll through the file independently.
        self.buffer.clamp_scroll(text_h);
        if viewport_changed || self.last_drawn_cursor != Some(self.buffer.cursor) {
            self.buffer.ensure_visible(text_h, text_w);
        }
        self.last_drawn_cursor = Some(self.buffer.cursor);
        let muted = self.pal().muted;
        let warning = self.pal().warning;

        // With wrapping every logical line may occupy several visual
        // rows; `scroll.y` is then a visual row, the gutter shows the
        // line number only on the first row of each line, and the caret
        // sits on the visual row holding the cursor.
        if self.buffer.wrap {
            let width = self.buffer.wrap_width.max(1);
            let mut rows: Vec<Line> = Vec::with_capacity(text_h);
            let mut caret_style = None;
            // the logical line and chunk the viewport starts on
            let (mut y, mut chunk_k) = self
                .buffer
                .vrow_position(self.buffer.scroll.1)
                .unwrap_or((self.buffer.lines.len(), 0));
            let mut remaining = text_h;
            while remaining > 0 && y < self.buffer.lines.len() {
                let line = &self.buffer.lines[y];
                let ops = self.highlighter.highlight_line(&self.buffer.lines, y);
                // selection overlap on this line, in byte offsets
                let sel = self
                    .buffer
                    .selection_on_line(y)
                    .map(|(a, b)| (char_index_to_byte(line, a), char_index_to_byte(line, b)));
                // search matches on this line, in char offsets
                let search_matches: Vec<(usize, usize, bool)> = self
                    .search
                    .as_ref()
                    .map(|s| s.matches_on_line(y).collect())
                    .unwrap_or_default();
                // the chunk the cursor sits on within this line
                let cursor_chunk = if self.focus == Focus::Editor
                    && self.save_as_input.is_none()
                    && self.goto_line_input.is_none()
                    && self.buffer.cursor.1 == y
                {
                    Some(visual_row_of(line, self.buffer.cursor.0, width))
                } else {
                    None
                };
                loop {
                    if remaining == 0 {
                        break;
                    }
                    let (cstart, cend) = visual_chunk(line, chunk_k, width);
                    let num = if chunk_k == 0 {
                        Span::styled(
                            format!("{:>width$} ", y + 1, width = gutter_w - 1),
                            Style::default().fg(muted),
                        )
                    } else {
                        Span::raw(" ".repeat(gutter_w))
                    };
                    if cursor_chunk == Some(chunk_k) {
                        let cursor_byte = char_index_to_byte(line, self.buffer.cursor.0);
                        caret_style = Some(
                            ops.iter()
                                .find(|(_, range)| range.contains(&cursor_byte))
                                .and_then(|(style, _)| *style)
                                .unwrap_or_default(),
                        );
                    }
                    let mut spans = vec![num];
                    let chunk_cols = line
                        .chars()
                        .skip(cstart)
                        .take(cend.saturating_sub(cstart))
                        .map(char_width)
                        .sum();
                    spans.extend(clip_ops(
                        line,
                        ops,
                        cstart,
                        chunk_cols,
                        sel,
                        &search_matches,
                        self.color_support,
                    ));
                    rows.push(Line::from(spans));
                    remaining -= 1;
                    if cend >= line.chars().count() {
                        y += 1;
                        chunk_k = 0;
                        break;
                    }
                    chunk_k += 1;
                }
            }
            if rows.is_empty() {
                rows.push(Line::from(Span::styled(
                    "(empty)",
                    Style::default().fg(self.pal().muted),
                )));
            }

            let paragraph = Paragraph::new(rows);
            frame.render_widget(paragraph, inner);

            if self.focus == Focus::Editor && !self.text_prompt_active() {
                let caret_vrow = self.buffer.cursor_vrow();
                if caret_vrow >= self.buffer.scroll.1 && caret_vrow < self.buffer.scroll.1 + text_h
                {
                    let cx = area.x + 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
                    let cy = area.y + 1 + (caret_vrow - self.buffer.scroll.1) as u16;
                    let inner_right = area.x + area.width.saturating_sub(1);
                    let inner_bottom = area.y + area.height.saturating_sub(1);
                    if cx < inner_right && cy < inner_bottom {
                        let symbol = self
                            .buffer
                            .lines
                            .get(self.buffer.cursor.1)
                            .and_then(|line| line.chars().nth(self.buffer.cursor.0))
                            .map_or_else(
                                || " ".to_string(),
                                |c| {
                                    if c == '\t' {
                                        " ".to_string()
                                    } else {
                                        c.to_string()
                                    }
                                },
                            );
                        let caret = Paragraph::new(Span::styled(
                            symbol,
                            self.paint_style(caret_style.unwrap_or_default().bg(warning)),
                        ));
                        frame.render_widget(caret, Rect::new(cx, cy, 1, 1));
                    }
                }
            }
            self.draw_editor_scrollbar(frame, area, text_h);
            return;
        }

        let start = self.buffer.scroll.1;
        let end = (start + text_h).min(self.buffer.lines.len());
        let mut rows: Vec<Line> = Vec::with_capacity(end.saturating_sub(start));
        let mut caret_style = None;
        for y in start..end {
            let num = Span::styled(
                format!("{:>width$} ", y + 1, width = gutter_w - 1),
                Style::default().fg(self.pal().muted),
            );
            let ops = self.highlighter.highlight_line(&self.buffer.lines, y);
            let line = &self.buffer.lines[y];
            if self.focus == Focus::Editor
                && self.save_as_input.is_none()
                && self.goto_line_input.is_none()
                && self.buffer.cursor.1 == y
                && self.buffer.cursor.0 >= self.buffer.scroll.0
                && self.buffer.cursor.0 < self.buffer.scroll.0 + text_w
            {
                let cursor_byte = char_index_to_byte(line, self.buffer.cursor.0);
                caret_style = Some(
                    ops.iter()
                        .find(|(_, range)| range.contains(&cursor_byte))
                        .and_then(|(style, _)| *style)
                        .unwrap_or_default(),
                );
            }
            // selection overlap on this line, in byte offsets
            let sel = self
                .buffer
                .selection_on_line(y)
                .map(|(a, b)| (char_index_to_byte(line, a), char_index_to_byte(line, b)));
            // search matches on this line, in char offsets
            let search_matches: Vec<(usize, usize, bool)> = self
                .search
                .as_ref()
                .map(|s| s.matches_on_line(y).collect())
                .unwrap_or_default();
            let mut spans = vec![num];
            spans.extend(clip_ops(
                line,
                ops,
                self.buffer.scroll.0,
                text_w,
                sel,
                &search_matches,
                self.color_support,
            ));
            rows.push(Line::from(spans));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(self.pal().muted),
            )));
        }

        let paragraph = Paragraph::new(rows);
        frame.render_widget(paragraph, inner);

        if self.focus == Focus::Editor && !self.text_prompt_active() {
            // Ratatui can position the terminal cursor, but cannot give it a
            // color. Render a block caret ourselves so it remains distinct
            // from the themed selection style (and leave the native cursor
            // hidden). Independent scrolling can put the caret outside the
            // viewport, in which case there is deliberately nothing to draw.
            let cursor_visible = self.buffer.cursor.1 >= self.buffer.scroll.1
                && self.buffer.cursor.1 < self.buffer.scroll.1.saturating_add(text_h)
                && self.buffer.cursor.0 >= self.buffer.scroll.0
                && self.buffer.cursor.0 <= self.buffer.scroll.0.saturating_add(text_w);
            let cx = area.x + 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
            let inner_right = area.x + area.width.saturating_sub(1);
            let inner_bottom = area.y + area.height.saturating_sub(1);
            if cursor_visible && cx < inner_right {
                let cy = area.y + 1 + (self.buffer.cursor.1 - self.buffer.scroll.1) as u16;
                if cy < inner_bottom {
                    let symbol = self
                        .buffer
                        .lines
                        .get(self.buffer.cursor.1)
                        .and_then(|line| line.chars().nth(self.buffer.cursor.0))
                        .map_or_else(
                            || " ".to_string(),
                            |c| {
                                if c == '\t' {
                                    " ".to_string()
                                } else {
                                    c.to_string()
                                }
                            },
                        );
                    let caret = Paragraph::new(Span::styled(
                        symbol,
                        self.paint_style(caret_style.unwrap_or_default().bg(warning)),
                    ));
                    frame.render_widget(caret, Rect::new(cx, cy, 1, 1));
                }
            }
        }
        self.draw_editor_scrollbar(frame, area, text_h);
    }

    /// Draw the vertical editor scrollbar over the block's right border. The
    /// border column is used rather than the text area, so adding the
    /// scrollbar does not steal a column from the editor or change wrapping.
    pub(super) fn draw_editor_scrollbar(&self, frame: &mut Frame, area: Rect, viewport: usize) {
        let content_length = self.buffer.total_visual_rows();
        let viewport = viewport.max(1);
        if content_length <= viewport || area.width == 0 || area.height <= 2 {
            return;
        }

        let scrollbar_area = Rect::new(area.x + area.width - 1, area.y + 1, 1, area.height - 2);
        // Ratatui's scrollbar position spans the number of possible
        // positions, not the number of content rows. Supplying the number
        // of viewport starts makes `scroll == content - viewport` land on
        // the final scrollbar cell instead of leaving a gap below the thumb.
        let scroll_positions = content_length.saturating_sub(viewport).saturating_add(1);
        let mut state = ScrollbarState::new(scroll_positions)
            .position(self.buffer.scroll.1)
            .viewport_content_length(viewport);
        let thumb_style = if self.focus == Focus::Editor {
            Style::default().fg(self.pal().accent)
        } else {
            Style::default().fg(self.pal().muted)
        };
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .track_style(Style::default().fg(self.pal().muted))
            .thumb_style(thumb_style);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut state);
    }
}
