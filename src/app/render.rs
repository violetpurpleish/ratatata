//! Main layout, shortcut bar, and directory sidebar rendering.

use super::*;

impl App {
    // ---- drawing -----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        // Paint the terminal with the theme's base color first. Individual
        // widgets and syntax spans then layer their semantic foregrounds and
        // backgrounds over it.
        let mut base = Style::default().bg(self.pal().bg);
        if self.color_support != ColorSupport::TrueColor {
            base = base.fg(self.pal().fg);
        }
        frame.render_widget(Block::default().style(base), frame.area());

        let pills = self.shortcut_pills();
        let [top_area, main, status_area] = Layout::vertical([
            Constraint::Length(shortcut_bar_height(&pills, frame.area().width)),
            Constraint::Min(0),
            Constraint::Length(STATUS_HEIGHT),
        ])
        .areas(frame.area());
        let (side_area, edit_area) = if self.sidebar_visible {
            let [side, edit] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
                    .areas(main);
            (side, edit)
        } else {
            (Rect::new(0, 0, 0, 0), main)
        };
        self.topbar_area = top_area;
        self.sidebar_area = side_area;
        self.editor_area = edit_area;

        self.draw_topbar(frame, top_area, &pills);
        if self.sidebar_visible {
            self.draw_sidebar(frame, side_area);
        }
        self.draw_editor(frame, edit_area);
        self.draw_status(frame, status_area);
    }

    /// The shortcut buttons shown in the top bar for the current mode:
    /// the full editing set, or the short preview-only set while an image
    /// is open.
    pub(super) fn shortcut_pills(&self) -> Vec<Shortcut> {
        if self.image.is_some() {
            vec![
                Shortcut::ClosePreview,
                Shortcut::Reload,
                Shortcut::SwitchFocus,
                Shortcut::Quit,
            ]
        } else {
            vec![
                Shortcut::SwitchFocus,
                Shortcut::ToggleSidebar,
                Shortcut::ToggleHidden,
                Shortcut::ToggleWrap,
                Shortcut::NewFile,
                Shortcut::Reload,
                Shortcut::Undo,
                Shortcut::Redo,
                Shortcut::Copy,
                Shortcut::Cut,
                Shortcut::Paste,
                Shortcut::SelectAll,
                Shortcut::Find,
                Shortcut::Replace,
                Shortcut::GoToLine,
                Shortcut::Save,
                Shortcut::Quit,
            ]
        }
    }

    /// The clickable shortcut buttons ("Ctrl+O switch", …) across the top,
    /// separated by " | " and indented one cell from the window edge.
    /// Buttons wrap onto extra rows when they don't fit. Quit is pinned
    /// so it is never dropped; any other overflow is marked with an
    /// ellipsis. Button rectangles from the last draw are kept for mouse
    /// hit-testing.
    pub(super) fn draw_topbar(&mut self, frame: &mut Frame, area: Rect, pills: &[Shortcut]) {
        self.topbar_buttons.clear();
        let hovered = self.hovered;
        let layout = wrap_topbar_pills(pills, area.width);
        let n_rows = layout.pills.last().map(|p| p.row + 1).unwrap_or(1).max(1);
        let last_body_row = layout
            .pills
            .iter()
            .rev()
            .find(|p| p.action != Shortcut::Quit)
            .map(|p| p.row);

        for row in 0..n_rows {
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            let mut first = true;
            for pill in layout.pills.iter().filter(|p| p.row == row) {
                if !first {
                    spans.push(Span::styled(
                        TOPBAR_SEPARATOR,
                        Style::default().fg(self.pal().muted),
                    ));
                }
                first = false;
                self.topbar_buttons.push((
                    pill.action,
                    Rect::new(area.x + pill.x, area.y + row, pill.width, 1),
                ));
                spans.extend(pill_spans(
                    pill.action,
                    hovered == Some(pill.action),
                    self.color_support,
                ));
            }
            if layout.truncated
                && last_body_row == Some(row)
                && !layout
                    .pills
                    .iter()
                    .any(|p| p.row == row && p.action == Shortcut::Quit)
            {
                spans.push(Span::styled("…", Style::default().fg(self.pal().muted)));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(area.x, area.y + row, area.width, 1),
            );
        }
    }

    pub(super) fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        self.sidebar_height = area.height;
        let inner_h = area.height.saturating_sub(2) as usize;
        self.sidebar.ensure_selected_visible(inner_h);

        let mut rows = Vec::with_capacity(self.sidebar.entries.len());
        for (i, entry) in self.sidebar.entries.iter().enumerate() {
            let display = match entry.kind {
                Kind::Parent | Kind::Dir => format!("{}/", entry.name),
                Kind::File => entry.name.clone(),
            };
            let mut style = match entry.kind {
                Kind::Parent => Style::default().fg(self.pal().muted),
                Kind::Dir => Style::default().fg(self.pal().info),
                Kind::File => Style::default().fg(self.pal().fg),
            };
            if entry.is_hidden() {
                style = style.add_modifier(Modifier::DIM);
            }
            let selected = i == self.sidebar.selected;
            if selected {
                style = self.highlight_style(style, self.pal().selection);
            }
            let marker = if selected { "▶ " } else { "  " };
            rows.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(self.pal().warning)),
                Span::styled(display, style),
            ]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(self.pal().muted),
            )));
        }

        let title = truncate(&self.sidebar.dir.display().to_string(), area.width as usize);
        let block = self.pane_block(title, PALETTE.info, self.focus == Focus::Sidebar);
        let paragraph = Paragraph::new(rows)
            .block(block)
            .scroll((self.sidebar.scroll as u16, 0));
        frame.render_widget(paragraph, area);
    }
}
