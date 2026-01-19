use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};

use super::theme;

pub enum TagEditorResult {
    Continue,
    Save(Vec<String>),
    Cancel,
}

pub struct TagEditor {
    pub file_index: usize,
    pub hunk_index: usize,
    filename: String,
    line_range: (usize, usize),
    input: String,
    available_tags: Vec<String>,
    filtered_indices: Vec<usize>,
    selected_index: usize,
    selected_tags: Vec<String>,
}

impl TagEditor {
    pub fn new(
        file_index: usize,
        hunk_index: usize,
        filename: String,
        line_range: (usize, usize),
        current_tags: Vec<String>,
        available_tags: Vec<String>,
    ) -> Self {
        let mut editor = Self {
            file_index,
            hunk_index,
            filename,
            line_range,
            input: String::new(),
            available_tags,
            filtered_indices: Vec::new(),
            selected_index: 0,
            selected_tags: normalize_tags(current_tags),
        };
        editor.available_tags = normalize_tags(editor.available_tags);
        editor.refresh_filter();
        editor
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> TagEditorResult {
        match key.code {
            KeyCode::Esc => TagEditorResult::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                TagEditorResult::Cancel
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
                TagEditorResult::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_index + 1 < self.filtered_indices.len() {
                    self.selected_index += 1;
                }
                TagEditorResult::Continue
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.refresh_filter();
                TagEditorResult::Continue
            }
            KeyCode::Char(' ') => {
                self.toggle_selected_tag();
                TagEditorResult::Continue
            }
            KeyCode::Enter => {
                if !self.input.trim().is_empty() {
                    self.add_tag(self.input.trim().to_string());
                }
                TagEditorResult::Save(self.selected_tags.clone())
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.input.push(ch);
                    self.refresh_filter();
                }
                TagEditorResult::Continue
            }
            _ => TagEditorResult::Continue,
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let t = theme::get();
        let area = frame.area();

        let width = 64.min(area.width.saturating_sub(4));
        let height = 12.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let short_filename = self.filename.rsplit('/').next().unwrap_or(&self.filename);
        let title = format!(
            " {} · L{}-{} ",
            short_filename, self.line_range.0, self.line_range.1
        );

        let block = Block::default()
            .title(title)
            .title_style(Style::default().fg(t.ui.text_secondary))
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(t.ui.border_focused))
            .style(Style::default().bg(t.ui.bg));
        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(inner);

        let tags_line = if self.selected_tags.is_empty() {
            "Tags: (none)".to_string()
        } else {
            format!("Tags: {}", self.selected_tags.join(", "))
        };
        let tags_paragraph = Paragraph::new(Line::from(Span::styled(
            tags_line,
            Style::default().fg(t.ui.text_primary),
        )))
        .style(Style::default().bg(t.ui.bg));
        frame.render_widget(tags_paragraph, chunks[0]);

        let input_line = Line::from(vec![
            Span::styled("Filter: ", Style::default().fg(t.ui.text_muted)),
            Span::styled(&self.input, Style::default().fg(t.ui.text_primary)),
            Span::styled("_", Style::default().fg(t.ui.text_muted)),
        ]);
        let input_paragraph = Paragraph::new(input_line).style(Style::default().bg(t.ui.bg));
        frame.render_widget(input_paragraph, chunks[1]);

        let items: Vec<ListItem> = if self.filtered_indices.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "(no tags)",
                Style::default().fg(t.ui.text_muted),
            )))]
        } else {
            self.filtered_indices
                .iter()
                .map(|&idx| {
                    let tag = &self.available_tags[idx];
                    let selected = self.selected_tags.contains(tag);
                    let marker = if selected { "[x]" } else { "[ ]" };
                    ListItem::new(Line::from(vec![
                        Span::styled(marker, Style::default().fg(t.ui.text_muted)),
                        Span::raw(" "),
                        Span::styled(tag, Style::default().fg(t.ui.text_primary)),
                    ]))
                })
                .collect()
        };

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .fg(t.ui.text_primary)
                    .bg(t.ui.selection_bg),
            )
            .highlight_symbol("› ");

        let mut state = ratatui::widgets::ListState::default();
        if !self.filtered_indices.is_empty() {
            state.select(Some(self.selected_index.min(self.filtered_indices.len() - 1)));
        }
        frame.render_stateful_widget(list, chunks[2], &mut state);

        let footer = Line::from(vec![
            Span::styled("space", Style::default().fg(t.ui.text_muted)),
            Span::styled(" toggle  ", Style::default().fg(t.ui.text_muted)),
            Span::styled("│  ", Style::default().fg(t.ui.border_unfocused)),
            Span::styled("enter", Style::default().fg(t.ui.text_muted)),
            Span::styled(" save  ", Style::default().fg(t.ui.text_muted)),
            Span::styled("│  ", Style::default().fg(t.ui.border_unfocused)),
            Span::styled("esc", Style::default().fg(t.ui.text_muted)),
            Span::styled(" cancel", Style::default().fg(t.ui.text_muted)),
        ]);
        let footer_paragraph = Paragraph::new(footer)
            .style(Style::default().bg(t.ui.bg))
            .alignment(Alignment::Center);
        frame.render_widget(footer_paragraph, chunks[3]);
    }

    fn refresh_filter(&mut self) {
        let query = self.input.to_lowercase();
        self.filtered_indices = self
            .available_tags
            .iter()
            .enumerate()
            .filter(|(_, tag)| tag.to_lowercase().contains(&query))
            .map(|(idx, _)| idx)
            .collect();
        if self.selected_index >= self.filtered_indices.len() {
            self.selected_index = 0;
        }
    }

    fn toggle_selected_tag(&mut self) {
        if let Some(tag) = self.selected_tag_from_list() {
            if let Some(pos) = self.selected_tags.iter().position(|t| t == tag) {
                self.selected_tags.remove(pos);
            } else {
                self.selected_tags.push(tag.to_string());
                self.selected_tags = normalize_tags(self.selected_tags.clone());
            }
        }
    }

    fn add_tag(&mut self, tag: String) {
        if !self.selected_tags.contains(&tag) {
            self.selected_tags.push(tag.clone());
            self.selected_tags = normalize_tags(self.selected_tags.clone());
        }
        if !self.available_tags.contains(&tag) {
            self.available_tags.push(tag);
            self.available_tags = normalize_tags(self.available_tags.clone());
        }
        self.input.clear();
        self.refresh_filter();
    }

    fn selected_tag_from_list(&self) -> Option<&str> {
        self.filtered_indices
            .get(self.selected_index)
            .and_then(|&idx| self.available_tags.get(idx).map(|s| s.as_str()))
    }
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<String> = tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}
