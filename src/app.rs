//! Application state, key handling and rendering.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::buffer::Buffer;
use crate::sidebar::{Kind, Sidebar};

/// How long transient status messages stay visible.
const MESSAGE_TTL: Duration = Duration::from_secs(4);

const SIDEBAR_WIDTH: u16 = 28;
const STATUS_HEIGHT: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Editor,
}

pub struct App {
    pub buffer: Buffer,
    pub sidebar: Sidebar,
    pub focus: Focus,
    pub should_quit: bool,
    /// Transient status message with expiry.
    message: Option<(String, Instant)>,
    /// Active "save as" input text, when the buffer has no file name.
    save_as_input: Option<String>,
    /// Set when Ctrl+Q is pressed with unsaved changes; second press quits.
    quit_armed: bool,
    /// Viewport sizes from the last draw, used for paging and scrolling.
    editor_text: (u16, u16),
    sidebar_height: u16,
}

impl App {
    pub fn new(dir: PathBuf, file: Option<PathBuf>) -> io::Result<Self> {
        let buffer = match file {
            Some(path) => Buffer::from_path(path)?,
            None => Buffer::empty(),
        };
        let focus = if buffer.path.is_some() {
            Focus::Editor
        } else {
            Focus::Sidebar
        };
        let mut sidebar = Sidebar::new(dir)?;
        if let Some(name) = buffer
            .path
            .as_ref()
            .and_then(|p| p.file_name())
        {
            sidebar.select_name(&name.to_string_lossy());
        }
        Ok(Self {
            buffer,
            sidebar,
            focus,
            should_quit: false,
            message: None,
            save_as_input: None,
            quit_armed: false,
            editor_text: (0, 0),
            sidebar_height: 0,
        })
    }

    fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now() + MESSAGE_TTL));
    }

    // ---- key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Global shortcuts work everywhere, even inside the save-as prompt.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('q') => {
                    if self.buffer.dirty && !self.quit_armed {
                        self.quit_armed = true;
                        self.set_message("unsaved changes — press Ctrl+Q again to quit anyway");
                    } else {
                        self.should_quit = true;
                    }
                    return;
                }
                KeyCode::Char('s') => {
                    self.save();
                    return;
                }
                KeyCode::Char('o') => {
                    if self.save_as_input.is_none() {
                        self.focus = match self.focus {
                            Focus::Sidebar => Focus::Editor,
                            Focus::Editor => Focus::Sidebar,
                        };
                    }
                    return;
                }
                _ => return,
            }
        }

        if let Some(input) = self.save_as_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.save_as_input = None,
                KeyCode::Enter => self.confirm_save_as(),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => input.push(c),
                _ => {}
            }
            return;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Editor => self.handle_editor_key(key),
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        let page = self.sidebar_height.saturating_sub(2) as i64;
        match key.code {
            KeyCode::Up => self.sidebar.move_selection(-1),
            KeyCode::Down => self.sidebar.move_selection(1),
            KeyCode::Home => self.sidebar.selected = 0,
            KeyCode::End => {
                self.sidebar.selected = self.sidebar.entries.len().saturating_sub(1);
            }
            KeyCode::PageUp => self.sidebar.move_selection(-page.max(1)),
            KeyCode::PageDown => self.sidebar.move_selection(page.max(1)),
            KeyCode::Enter => self.open_selected(),
            KeyCode::Backspace => match self.sidebar.ascend() {
                Ok(()) => {}
                Err(e) => self.set_message(format!("cannot go up: {e}")),
            },
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        let (w, h) = self.editor_text;
        let page = h.max(1) as usize;
        match key.code {
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if !c.is_control() {
                    self.buffer.insert_char(c);
                }
            }
            KeyCode::Enter => self.buffer.newline(),
            KeyCode::Tab => self.buffer.insert_text("    "),
            KeyCode::Backspace => self.buffer.backspace(),
            KeyCode::Delete => self.buffer.delete(),
            KeyCode::Left => self.buffer.move_left(),
            KeyCode::Right => self.buffer.move_right(),
            KeyCode::Up => self.buffer.move_up(),
            KeyCode::Down => self.buffer.move_down(),
            KeyCode::Home => self.buffer.home(),
            KeyCode::End => self.buffer.end(),
            KeyCode::PageUp => self.buffer.page_up(page),
            KeyCode::PageDown => self.buffer.page_down(page),
            _ => {}
        }
        self.quit_armed = false;
        self.buffer.ensure_visible(h as usize, w as usize);
    }

    // ---- file operations ---------------------------------------------------

    fn save(&mut self) {
        if self.save_as_input.is_some() {
            return;
        }
        if self.buffer.path.is_some() {
            match self.buffer.save() {
                Ok(()) => {
                    self.quit_armed = false;
                    let path = self.buffer.path.as_ref().unwrap().display();
                    self.set_message(format!("saved {path}"));
                }
                Err(e) => self.set_message(format!("save failed: {e}")),
            }
        } else {
            // no file name yet: ask for one
            self.save_as_input = Some(String::new());
        }
    }

    fn confirm_save_as(&mut self) {
        let Some(input) = self.save_as_input.take() else {
            return;
        };
        let input = input.trim().to_string();
        if input.is_empty() {
            return;
        }
        let path = PathBuf::from(&input);
        if path.is_dir() {
            self.set_message(format!("{input} is a directory"));
            return;
        }
        self.buffer.path = Some(path.clone());
        match self.buffer.save() {
            Ok(()) => {
                self.quit_armed = false;
                // refresh the sidebar so the new file shows up
                if let Err(e) = self.sidebar.reload() {
                    self.set_message(format!("saved {}, but sidebar refresh failed: {e}", path.display()));
                    return;
                }
                self.set_message(format!("saved {}", path.display()));
            }
            Err(e) => self.set_message(format!("save failed: {e}")),
        }
    }

    fn open_selected(&mut self) {
        let Some(path) = self.sidebar.selected_path() else {
            return;
        };
        if self.sidebar.selected_is_dir() {
            match self.sidebar.descend() {
                Ok(()) => {}
                Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
            }
            return;
        }
        if self.buffer.dirty {
            self.set_message("unsaved changes — press Ctrl+S to save first");
            return;
        }
        match Buffer::from_path(path.clone()) {
            Ok(buffer) => {
                self.buffer = buffer;
                self.quit_armed = false;
                self.focus = Focus::Editor;
                self.set_message(format!("opened {}", path.display()));
            }
            Err(e) => self.set_message(format!("cannot open {}: {e}", path.display())),
        }
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&mut self, frame: &mut Frame) {
        let [main, status_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(STATUS_HEIGHT)])
                .areas(frame.area());
        let [side_area, edit_area] = Layout::horizontal([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Min(0),
        ])
        .areas(main);

        self.draw_sidebar(frame, side_area);
        self.draw_editor(frame, edit_area);
        self.draw_status(frame, status_area);
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
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
                Kind::Parent => Style::default().fg(Color::DarkGray),
                Kind::Dir => Style::default().fg(Color::Cyan),
                Kind::File => Style::default().fg(Color::Gray),
            };
            if entry.is_hidden() {
                style = style.add_modifier(Modifier::DIM);
            }
            let selected = i == self.sidebar.selected;
            if selected {
                style = style.bg(Color::DarkGray);
            }
            let marker = if selected { "▶ " } else { "  " };
            rows.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled(display, style),
            ]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let title = truncate(&self.sidebar.dir.display().to_string(), area.width as usize);
        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(Color::Cyan)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Sidebar {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let paragraph = Paragraph::new(rows)
            .block(block)
            .scroll((self.sidebar.scroll as u16, 0));
        frame.render_widget(paragraph, area);

        // put the terminal cursor on the selected row
        let row = (self.sidebar.selected - self.sidebar.scroll) as u16;
        frame.set_cursor_position(Position::new(area.x + 1, area.y + 1 + row));
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let title = self
            .buffer
            .path
            .as_ref()
            .map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let title = truncate(&title, area.width as usize);

        let block = Block::bordered()
            .title(Span::styled(title, Style::default().fg(Color::Green)))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(if self.focus == Focus::Editor {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let inner = block.inner(area);

        let gutter_w = self.buffer.lines.len().to_string().len() + 1;
        let text_w = inner.width.saturating_sub(gutter_w as u16) as usize;
        let text_h = inner.height as usize;
        self.editor_text = (text_w as u16, text_h as u16);
        self.buffer.ensure_visible(text_h, text_w);

        let start = self.buffer.scroll.1;
        let end = (start + text_h).min(self.buffer.lines.len());
        let mut rows = Vec::with_capacity(end - start);
        for y in start..end {
            let num = Span::styled(
                format!("{:>width$} ", y + 1, width = gutter_w - 1),
                Style::default().fg(Color::DarkGray),
            );
            let text = Span::raw(self.buffer.visible_line(y, text_w));
            rows.push(Line::from(vec![num, text]));
        }
        if rows.is_empty() {
            rows.push(Line::from(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let paragraph = Paragraph::new(rows).block(block);
        frame.render_widget(paragraph, area);

        if self.focus == Focus::Editor && self.save_as_input.is_none() {
            let cx = 1 + gutter_w as u16 + self.buffer.cursor_col() as u16;
            let cy = 1 + (self.buffer.cursor.1 - self.buffer.scroll.1) as u16;
            frame.set_cursor_position(Position::new(area.x + cx, area.y + cy));
        }
    }

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        // "save as" prompt replaces the status bar content
        if let Some(input) = &self.save_as_input {
            let prompt = "save as: ";
            let prompt_w = prompt.width() as u16;
            let input_w = input.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() as u16;
            let paragraph = Paragraph::new(Line::from(vec![
                Span::styled(prompt, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(input.clone(), Style::default().fg(Color::White)),
            ]))
            .style(Style::default().bg(Color::Rgb(30, 30, 30)));
            frame.render_widget(paragraph, area);
            frame.set_cursor_position(Position::new(area.x + prompt_w + input_w, area.y));
            return;
        }

        // right: position + help
        let (x, y) = self.buffer.cursor;
        let right = format!(
            "{}:{}   Ctrl+O switch · Ctrl+S save · Ctrl+Q quit",
            y + 1,
            x + 1
        );
        let right_width = right.width() as u16;

        let [left_area, right_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(right_width),
        ])
        .areas(area);
        let base = Style::default().bg(Color::Rgb(30, 30, 30)).fg(Color::Gray);

        // left: focus + file + modified state, or a transient message
        let (left_spans, left_style): (Vec<Span>, Style) = if let Some((msg, expiry)) = &self.message {
            if *expiry > Instant::now() {
                // keep the modified indicator visible even while a message
                // is showing
                let mut spans = vec![Span::styled(
                    msg.clone(),
                    Style::default().fg(Color::Yellow),
                )];
                if self.buffer.dirty {
                    spans.push(Span::styled(
                        " ● modified",
                        Style::default()
                            .fg(Color::Yellow)
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
            Paragraph::new(Line::from(right)).style(base).alignment(ratatui::layout::Alignment::Right),
            right_area,
        );
    }

    /// The normal (non-message) left side of the status bar. Truncates the
    /// path so the focus tag and the modified indicator always stay visible.
    fn status_left(&self, width: u16) -> (Vec<Span<'static>>, Style) {
        let (tag, tag_color) = match self.focus {
            Focus::Sidebar => ("SIDEBAR", Color::Cyan),
            Focus::Editor => ("EDITOR", Color::Green),
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
        let path_max = width.saturating_sub(tag.width() as u16 + dirty.width() as u16 + 2);
        let path = truncate(&path, path_max as usize);
        (
            vec![
                Span::styled(tag, Style::default().fg(tag_color).add_modifier(Modifier::BOLD)),
                Span::raw(format!(" {path} ")),
                Span::styled(
                    dirty,
                    if self.buffer.dirty {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Green)
                    },
                ),
            ],
            Style::default(),
        )
    }
}

/// Shorten `s` to at most `max` chars, keeping the end and prefixing "…".
fn truncate(s: &str, max: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use std::fs;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("ratata-app-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn opening_a_file_arg_focuses_editor() {
        let dir = scratch("filearg");
        let file = dir.join("a.txt");
        fs::write(&file, "hello\nworld\n").unwrap();
        let app = App::new(dir, Some(file)).unwrap();
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["hello", "world", ""]);
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn directory_arg_focuses_sidebar() {
        let dir = scratch("dira");
        let app = App::new(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.buffer.path.is_none());
    }

    #[test]
    fn ctrl_o_toggles_focus() {
        let dir = scratch("toggle");
        let mut app = App::new(dir, None).unwrap();
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Editor);
        app.handle_key(ctrl('o'));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn save_as_flow_creates_file_and_updates_sidebar() {
        let dir = scratch("saveas");
        let mut app = App::new(dir.clone(), None).unwrap();

        app.handle_key(ctrl('o')); // focus editor
        for c in "hello".chars() {            app.handle_key(char_key(c));
        }
        assert!(app.buffer.dirty);

        // Ctrl+S with no file name enters the save-as prompt
        app.handle_key(ctrl('s'));
        assert!(app.save_as_input.is_some());

        // typing while the prompt is active goes into the prompt, not the buffer
        let save_path = dir.join("out.txt");
        for c in save_path.display().to_string().chars() {
            app.handle_key(char_key(c));
        }
        assert_eq!(app.save_as_input.as_deref(), Some(save_path.to_str().unwrap()));
        assert_eq!(app.buffer.lines, vec!["hello"]);

        app.handle_key(key(KeyCode::Enter));
        assert!(app.save_as_input.is_none());
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(&save_path).unwrap(), "hello");
        // the sidebar was refreshed and now lists the new file
        assert!(
            app.sidebar
                .entries
                .iter()
                .any(|e| e.name == "out.txt")
        );
    }

    #[test]
    fn escape_cancels_save_as() {
        let dir = scratch("cancel");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(ctrl('s'));
        app.handle_key(char_key('x'));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.save_as_input.is_none());
        assert!(app.buffer.path.is_none());
    }

    #[test]
    fn sidebar_enter_opens_file_and_switches_focus() {
        let dir = scratch("open");
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = App::new(dir, None).unwrap();
        app.sidebar.select_name("b.txt");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffer.lines, vec!["beta"]);
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "b.txt"
        );
    }

    #[test]
    fn dirty_buffer_blocks_opening_another_file() {
        let dir = scratch("dirtyblock");
        fs::write(dir.join("a.txt"), "alpha").unwrap();
        fs::write(dir.join("b.txt"), "beta").unwrap();
        let mut app = App::new(dir.clone(), Some(dir.join("a.txt"))).unwrap();
        app.handle_key(char_key('X')); // make dirty
        assert!(app.buffer.dirty);

        app.sidebar.select_name("b.txt");
        app.handle_key(ctrl('o')); // sidebar
        app.handle_key(key(KeyCode::Enter));

        // still on a.txt with the edit intact (cursor starts at 0,0)
        assert_eq!(app.buffer.lines, vec!["Xalpha"]);
        assert_eq!(
            app.buffer.path.as_ref().unwrap().file_name().unwrap(),
            "a.txt"
        );
        assert!(app.message.is_some());
    }

    #[test]
    fn ctrl_s_saves_and_clears_dirty() {
        let dir = scratch("save");
        let file = dir.join("a.txt");
        fs::write(&file, "alpha").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();
        app.handle_key(char_key('Z'));
        assert!(app.buffer.dirty);
        app.handle_key(ctrl('s'));
        assert!(!app.buffer.dirty);
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "Zalpha");
    }

    #[test]
    fn ctrl_q_requires_second_press_when_dirty() {
        let dir = scratch("quit");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('q'));
        assert!(!app.should_quit); // blocked, message shown
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_q_quits_immediately_when_clean() {
        let dir = scratch("quit2");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn sidebar_backspace_ascends() {
        let dir = scratch("ascend");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut app = App::new(dir.clone(), None).unwrap();
        app.sidebar.select_name("sub");
        app.handle_key(key(KeyCode::Enter)); // descend
        assert_eq!(app.sidebar.dir, dir.join("sub"));
        app.handle_key(key(KeyCode::Backspace)); // ascend
        assert_eq!(app.sidebar.dir, dir);
    }

    // ---- headless rendering via ratatui's TestBackend --------------------

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App) -> Vec<String> {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect()
    }

    fn row_contains(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|r| r.contains(needle))
    }

    #[test]
    fn renders_sidebar_editor_and_status() {
        let dir = scratch("render1");
        fs::create_dir(dir.join("docs")).unwrap();
        fs::write(dir.join("notes.txt"), "hello world").unwrap();
        let mut app = App::new(dir.clone(), None).unwrap();

        let rows = render(&mut app);
        // sidebar: dirs and files listed, selected row marked
        assert!(row_contains(&rows, "docs/"));
        assert!(row_contains(&rows, "notes.txt"));
        assert!(row_contains(&rows, "▶"));
        // editor: untitled block, empty buffer with line number 1
        assert!(row_contains(&rows, "untitled"));
        assert!(row_contains(&rows, "1 "));
        // status bar: untitled + saved indicator + focus tag + help
        assert!(row_contains(&rows, "SIDEBAR"));
        assert!(row_contains(&rows, "○ saved"));
        assert!(row_contains(&rows, "Ctrl+S save"));
        assert!(row_contains(&rows, "1:1"));
    }

    #[test]
    fn renders_file_content_and_modified_state() {
        let dir = scratch("render2");
        let file = dir.join("notes.txt");
        fs::write(&file, "hello world").unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        // clean state shows the content and the saved marker
        let rows = render(&mut app);
        assert!(row_contains(&rows, "hello world"));
        assert!(row_contains(&rows, "EDITOR"));
        assert!(row_contains(&rows, "○ saved"));

        // type something -> modified marker appears
        app.handle_key(char_key('!'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "● modified"));

        // save -> message shown, and the marker returns once it expires
        app.handle_key(ctrl('s'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "saved "));
        app.message = None; // simulate the message expiring
        let rows = render(&mut app);
        assert!(row_contains(&rows, "○ saved"));
    }

    #[test]
    fn renders_save_as_prompt() {
        let dir = scratch("render3");
        let mut app = App::new(dir, None).unwrap();
        app.handle_key(ctrl('o'));
        app.handle_key(char_key('x'));
        app.handle_key(ctrl('s'));
        let rows = render(&mut app);
        assert!(row_contains(&rows, "save as:"));
    }

    #[test]
    fn renders_long_content_with_scrolling() {
        let dir = scratch("render4");
        let file = dir.join("long.txt");
        let content: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
        fs::write(&file, content).unwrap();
        let mut app = App::new(dir.clone(), Some(file)).unwrap();

        // cursor at the end of the buffer; drawing must scroll it into view
        // without panicking
        app.buffer.cursor = (200, 199);
        let rows = render(&mut app);
        assert!(row_contains(&rows, "line 199"));
        assert!(!row_contains(&rows, "line 000"));
    }
}
