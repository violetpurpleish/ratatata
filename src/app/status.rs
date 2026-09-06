//! Status messages, text prompts, and cursor/file status rendering.

use super::*;

impl App {
    pub(super) fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        // "save as" prompt replaces the status bar content
        if let Some(input) = &self.save_as_input {
            let prompt = if self.save_as_overwrite.as_ref() == Some(&PathBuf::from(input.trim())) {
                "exists — Enter overwrites, Esc cancels: "
            } else {
                self.save_as_error.as_deref().unwrap_or("save as: ")
            };
            let prompt_w = prompt.width() as u16;
            let input_w = input.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let paragraph = Paragraph::new(Line::from(vec![
                Span::styled(
                    prompt,
                    Style::default()
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(self.pal().fg)),
            ]))
            .style(Style::default().bg(self.pal().bg));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(
                area.x
                    .saturating_add(prompt_w)
                    .saturating_add(input_w)
                    .min(area.right().saturating_sub(1)),
                area.y,
            ));
            return;
        }

        // "go to line" prompt replaces the status bar content
        if let Some(input) = &self.goto_line_input {
            let prompt = "go to line: ";
            let prompt_w = prompt.width() as u16;
            let input_w = input.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let paragraph = Paragraph::new(Line::from(vec![
                Span::styled(
                    prompt,
                    Style::default()
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(input.clone(), Style::default().fg(self.pal().fg)),
            ]))
            .style(Style::default().bg(self.pal().bg));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(
                area.x
                    .saturating_add(prompt_w)
                    .saturating_add(input_w)
                    .min(area.right().saturating_sub(1)),
                area.y,
            ));
            return;
        }

        // the search / replace prompt replaces the status bar content while active
        if let Some(search) = &self.search {
            let editing_replacement =
                search.is_replace() && search.field == SearchField::Replacement;
            let (prompt, value) = if editing_replacement {
                ("replace: ", search.replacement.clone().unwrap_or_default())
            } else if search.is_replace() {
                ("find: ", search.query.clone())
            } else {
                ("search: ", search.query.clone())
            };
            let prompt_w = prompt.width() as u16;
            let input_w = value.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let base = Style::default().bg(self.pal().bg);
            let counter: Vec<Span> = if search.query.is_empty() {
                Vec::new()
            } else if search.match_count() == 0 {
                vec![Span::styled(
                    "no matches",
                    Style::default()
                        .fg(self.pal().warning)
                        .add_modifier(Modifier::BOLD),
                )]
            } else {
                vec![Span::styled(
                    format!("{}/{}", search.current_index() + 1, search.match_count()),
                    Style::default().fg(self.pal().info),
                )]
            };
            let counter_w = counter.iter().map(|s| s.content.width() as u16).sum();
            let [left_area, right_area] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(counter_w)]).areas(area);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        prompt,
                        Style::default()
                            .fg(self.pal().warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value, Style::default().fg(self.pal().fg)),
                ]))
                .style(base),
                left_area,
            );
            frame.render_widget(Paragraph::new(Line::from(counter)).style(base), right_area);
            let cursor_x = (area.x + prompt_w + input_w).min(left_area.x + left_area.width);
            frame.set_cursor_position(Position::new(cursor_x, area.y));
            return;
        }

        // right: cursor position, or a description of the shortcut button
        // currently hovered in the top bar
        let (x, y) = self.buffer.cursor;
        let right = match self.hovered {
            Some(action) => action.description().to_string(),
            None if self.image.is_some() => String::new(),
            None => format!("{}:{}", y + 1, x + 1),
        };
        // cap the help so the left side (focus, file, modified state) always
        // stays visible, even on narrow terminals
        let right_width = (right.width() as u16).min(area.width.saturating_sub(24));

        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
        let base = Style::default().bg(self.pal().bg).fg(self.pal().fg);

        // left: focus + file + modified state, or a transient message
        let (left_spans, left_style): (Vec<Span>, Style) =
            if let Some((msg, expiry)) = &self.message {
                if *expiry > Instant::now() {
                    // keep the modified indicator visible even while a message
                    // is showing
                    let mut spans = vec![Span::styled(
                        msg.clone(),
                        Style::default().fg(self.pal().warning),
                    )];
                    if self.buffer.dirty {
                        spans.push(Span::styled(
                            " ● modified",
                            Style::default()
                                .fg(self.pal().warning)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    (spans, Style::default())
                } else {
                    self.message = None;
                    self.status_left(left_area.width)
                }
            } else {
                self.status_left(left_area.width)
            };
        frame.render_widget(
            Paragraph::new(Line::from(left_spans)).style(base.patch(left_style)),
            left_area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(right))
                .style(base)
                .alignment(ratatui::layout::Alignment::Right),
            right_area,
        );
    }

    /// The normal (non-message) left side of the status bar. Truncates the
    /// path so the focus tag and the modified indicator always stay visible.
    pub(super) fn status_left(&self, width: u16) -> (Vec<Span<'static>>, Style) {
        if let Some(preview) = &self.image {
            let tag = "IMAGE";
            let path = preview.path.display().to_string();
            let dims = format!("[{}x{}]", preview.pixels.0, preview.pixels.1);
            let view = "○ view";
            let path_max = width
                .saturating_sub(tag.width() as u16 + dims.width() as u16 + view.width() as u16 + 2);
            let path = truncate(&path, path_max as usize);
            return (
                vec![
                    Span::styled(
                        tag,
                        Style::default()
                            .fg(self.pal().secondary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" {path} ")),
                    Span::styled(dims, Style::default().fg(self.pal().muted)),
                    Span::styled(view, Style::default().fg(self.pal().info)),
                ],
                Style::default(),
            );
        }
        let (tag, tag_color) = match self.focus {
            Focus::Sidebar => ("SIDEBAR", self.pal().info),
            Focus::Editor => ("EDITOR", self.pal().success),
        };
        let path = self
            .buffer
            .path
            .as_ref()
            .map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let dirty = if self.buffer.dirty {
            "● modified"
        } else {
            "○ saved"
        };
        let syntax = if self.highlighter.syntax_name() != "Plain Text" {
            format!("[{}] ", self.highlighter.syntax_name())
        } else {
            String::new()
        };
        let wrap = if self.buffer.wrap { "wrap " } else { "" };
        let path_max = width.saturating_sub(
            tag.width() as u16
                + dirty.width() as u16
                + syntax.width() as u16
                + wrap.width() as u16
                + 2,
        );
        let path = truncate(&path, path_max as usize);
        (
            vec![
                Span::styled(
                    tag,
                    Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {path} ")),
                Span::styled(syntax, Style::default().fg(self.pal().muted)),
                Span::styled(wrap, Style::default().fg(self.pal().info)),
                Span::styled(
                    dirty,
                    if self.buffer.dirty {
                        Style::default()
                            .fg(self.pal().warning)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(self.pal().success)
                    },
                ),
            ],
            Style::default(),
        )
    }
}
