use std::collections::BTreeSet;
use std::io::{self, Stderr};
use std::path::PathBuf;

use anyhow::Context;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::AnyResult;
use crate::git::{BaseAudit, DeleteKind, WorktreeAudit, WorktreeProtection, WorktreeState};

type Tui = Terminal<CrosstermBackend<Stderr>>;

#[derive(Debug, Default)]
pub(crate) struct WorktreeSelection {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) force: bool,
}

pub(crate) fn select_worktrees(worktrees: Vec<WorktreeAudit>) -> AnyResult<WorktreeSelection> {
    let mut terminal = start_terminal()?;
    let _guard = TerminalGuard;
    let mut app = App::new(worktrees);

    loop {
        terminal
            .draw(|frame| render(frame, &app))
            .context("failed to draw worktree selector")?;
        let Event::Key(key) = event::read().context("failed to read terminal input")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Outcome::Continue => {}
            Outcome::Cancel => return Ok(WorktreeSelection::default()),
            Outcome::Remove => {
                return Ok(WorktreeSelection {
                    paths: app.selected.into_iter().collect(),
                    force: app.force,
                });
            }
        }
    }
}

fn start_terminal() -> AnyResult<Tui> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stderr = io::stderr();
    if let Err(error) = execute!(stderr, EnterAlternateScreen, EnableMouseCapture, Hide) {
        let _ = disable_raw_mode();
        return Err(error).context("failed to enter alternate screen");
    }
    match Terminal::new(CrosstermBackend::new(stderr)) {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stderr(),
                LeaveAlternateScreen,
                DisableMouseCapture,
                Show
            );
            Err(error).context("failed to initialize terminal")
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stderr(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Continue,
    Cancel,
    Remove,
}

struct App {
    worktrees: Vec<WorktreeAudit>,
    selected: BTreeSet<PathBuf>,
    cursor: usize,
    confirming: bool,
    force: bool,
}

impl App {
    fn new(worktrees: Vec<WorktreeAudit>) -> Self {
        Self {
            worktrees,
            selected: BTreeSet::new(),
            cursor: 0,
            confirming: false,
            force: false,
        }
    }

    fn current(&self) -> Option<&WorktreeAudit> {
        self.worktrees.get(self.cursor)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.confirming {
            return match key.code {
                KeyCode::Enter => Outcome::Remove,
                KeyCode::Char('f') => {
                    self.force = !self.force;
                    Outcome::Continue
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.confirming = false;
                    Outcome::Continue
                }
                _ => Outcome::Continue,
            };
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Outcome::Cancel,
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                Outcome::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_down(1);
                Outcome::Continue
            }
            KeyCode::PageUp => {
                self.cursor = self.cursor.saturating_sub(10);
                Outcome::Continue
            }
            KeyCode::PageDown => {
                self.move_down(10);
                Outcome::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                Outcome::Continue
            }
            KeyCode::End => {
                self.cursor = self.worktrees.len().saturating_sub(1);
                Outcome::Continue
            }
            KeyCode::Char(' ') => {
                self.toggle_current();
                Outcome::Continue
            }
            KeyCode::Char('a') => {
                self.select_all();
                Outcome::Continue
            }
            KeyCode::Char('x') => {
                self.selected.clear();
                Outcome::Continue
            }
            KeyCode::Char('f') => {
                self.force = !self.force;
                Outcome::Continue
            }
            KeyCode::Enter if !self.selected.is_empty() => {
                self.confirming = true;
                Outcome::Continue
            }
            _ => Outcome::Continue,
        }
    }

    fn move_down(&mut self, amount: usize) {
        let last = self.worktrees.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(amount).min(last);
    }

    fn toggle_current(&mut self) {
        let Some(worktree) = self.current() else {
            return;
        };
        if worktree.protection.is_some() {
            return;
        }
        let path = worktree.path.clone();
        if !self.selected.remove(&path) {
            self.selected.insert(path);
        }
    }

    fn select_all(&mut self) {
        let selectable = self
            .worktrees
            .iter()
            .filter(|worktree| worktree.protection.is_none())
            .map(|worktree| worktree.path.clone())
            .collect::<Vec<_>>();
        if selectable.iter().all(|path| self.selected.contains(path)) {
            for path in selectable {
                self.selected.remove(&path);
            }
        } else {
            self.selected.extend(selectable);
        }
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();
    if area.width < 72 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("终端尺寸太小\n请调整到至少 72 × 20")
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Worktree 清理 "),
                ),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(14),
            Constraint::Length(3),
        ])
        .split(area);
    render_header(frame, rows[0], app);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(rows[1]);
    render_list(frame, columns[0], app);
    render_audit(frame, columns[1], app);
    render_footer(frame, rows[2], app);
    if app.confirming {
        render_confirmation(frame, area, app);
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let title = Line::from(vec![
        Span::styled(
            " Worktree 清理 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {} 个 worktree  │  已选 {} 个",
            app.worktrees.len(),
            app.selected.len()
        )),
    ]);
    frame.render_widget(
        Paragraph::new(title)
            .block(Block::default().borders(Borders::ALL))
            .alignment(Alignment::Left),
        area,
    );
}

fn render_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let items = app
        .worktrees
        .iter()
        .map(|worktree| {
            let (marker, marker_style) = if worktree.protection.is_some() {
                ("[-]", Style::default().fg(Color::DarkGray))
            } else if app.selected.contains(&worktree.path) {
                ("[x]", Style::default().fg(Color::Green))
            } else {
                ("[ ]", Style::default().fg(Color::Gray))
            };
            let (state, state_style) = state_badge(worktree);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), marker_style),
                Span::styled(state, state_style),
                Span::raw(&worktree.branch),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.cursor.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 状态    Branch ")
                .title_bottom(" ↑/↓ 移动 · Space 选择 "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 45, 60))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶");
    frame.render_stateful_widget(list, area, &mut state);
}

fn state_badge(worktree: &WorktreeAudit) -> (&'static str, Style) {
    match worktree.state {
        WorktreeState::Missing => ("丢失    ", Style::default().fg(Color::Yellow)),
        WorktreeState::Dirty => ("有改动  ", Style::default().fg(Color::LightRed)),
        WorktreeState::Clean => ("干净    ", Style::default().fg(Color::Green)),
    }
}

fn render_audit(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(worktree) = app.current() else {
        frame.render_widget(
            Paragraph::new("没有已注册的 worktree")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" 审计 ")),
            area,
        );
        return;
    };
    let protection = protection_text(worktree.protection.as_ref());
    let mut lines = vec![
        audit_line("路径", &worktree.path.display().to_string(), Color::White),
        audit_line("分支", &worktree.branch, Color::Cyan),
        audit_line("提交", &worktree.short_object, Color::Magenta),
        audit_line("状态", state_text(worktree), state_color(worktree)),
        audit_line("保护", protection, protection_color(worktree)),
        audit_line("主题", &worktree.subject, Color::White),
        Line::raw(""),
        Line::styled(
            "基准分支对比",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if worktree.bases.is_empty() {
        lines.push(Line::styled(
            "  未检测到可用的 branch 审计信息",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        lines.extend(worktree.bases.iter().map(base_line));
    }
    lines.extend([
        Line::styled(
            format!("相对 {} 的差异", worktree.diff_base),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("  {}", worktree.diffstat)),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" 审计详情 ")),
        area,
    );
}

fn audit_line(label: &str, value: &str, color: Color) -> Line<'static> {
    let label_width = label.chars().count() * 2;
    let padding = " ".repeat(10_usize.saturating_sub(label_width));
    Line::from(vec![
        Span::styled(
            format!("{label}{padding}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value.to_owned(), Style::default().fg(color)),
    ])
}

fn state_text(worktree: &WorktreeAudit) -> &'static str {
    match worktree.state {
        WorktreeState::Missing => "目录丢失 — 将清理注册信息",
        WorktreeState::Dirty => "有本地改动 — 禁止删除",
        WorktreeState::Clean => "干净 — 可以选择",
    }
}

fn state_color(worktree: &WorktreeAudit) -> Color {
    match worktree.state {
        WorktreeState::Missing => Color::Yellow,
        WorktreeState::Dirty => Color::LightRed,
        WorktreeState::Clean => Color::Green,
    }
}

fn protection_text(protection: Option<&WorktreeProtection>) -> &str {
    match protection {
        Some(WorktreeProtection::Main) => "锁定 — main worktree",
        Some(WorktreeProtection::Current) => "锁定 — 当前 worktree",
        Some(WorktreeProtection::Dirty) => "锁定 — 存在本地改动",
        Some(WorktreeProtection::Locked(_)) => "锁定 — Git worktree lock",
        None => "无 — 可以选择",
    }
}

fn protection_color(worktree: &WorktreeAudit) -> Color {
    if worktree.protection.is_some() {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn base_line(base: &BaseAudit) -> Line<'static> {
    let (status, color) = match base.absorption {
        Some(DeleteKind::Merged) => ("已合并", Color::Green),
        Some(DeleteKind::Equivalent) => ("内容等价", Color::LightGreen),
        None => ("无法确认", Color::LightRed),
    };
    Line::from(vec![
        Span::raw(format!("  {}  ", base.name)),
        Span::styled(status, Style::default().fg(color)),
        Span::raw(format!("  领先 {} / 落后 {}", base.ahead, base.behind)),
    ])
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let enter = if app.selected.is_empty() {
        "查看"
    } else {
        "确认"
    };
    let help = Line::from(vec![
        Span::styled(" a ", key_style()),
        Span::raw("全选  "),
        Span::styled(" x ", key_style()),
        Span::raw("清空  "),
        Span::styled(" f ", key_style()),
        Span::raw(if app.force {
            "[x] 强制  "
        } else {
            "[ ] 强制  "
        }),
        Span::styled(" Enter ", key_style()),
        Span::raw(format!("{enter}  ")),
        Span::styled(" q ", key_style()),
        Span::raw("退出"),
    ]);
    frame.render_widget(
        Paragraph::new(help)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn key_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

fn render_confirmation(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let popup = centered_rect(72, 60, area);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::styled(
            format!("要删除这 {} 个 linked worktree 吗？", app.selected.len()),
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let limit = popup.height.saturating_sub(11) as usize;
    for path in app.selected.iter().take(limit) {
        lines.push(Line::raw(format!("  • {}", path.display())));
    }
    let remaining = app.selected.len().saturating_sub(limit);
    if remaining > 0 {
        lines.push(Line::styled(
            format!("  … 还有 {remaining} 个 worktree"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.extend([
        Line::raw(""),
        Line::from(vec![
            Span::styled(" f ", key_style()),
            Span::raw(if app.force {
                "[x] 强制删除（使用 git worktree remove --force）"
            } else {
                "[ ] 强制删除"
            }),
        ]),
        Line::styled(
            if app.force {
                "强制模式可删除含 submodule 的 worktree；硬保护项仍不会删除。"
            } else {
                "含 submodule 的 worktree 可能需要勾选强制删除。"
            },
            Style::default().fg(if app.force {
                Color::LightRed
            } else {
                Color::Yellow
            }),
        ),
        Line::styled(
            "目录与 worktree 注册信息会被删除；对应 local branch 会保留。",
            Style::default().fg(Color::Yellow),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Enter ", key_style()),
            Span::raw("删除     "),
            Span::styled(" Esc ", key_style()),
            Span::raw("返回"),
        ])
        .alignment(Alignment::Center),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightRed))
                .title(" 确认删除 "),
        ),
        popup,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str, protection: Option<WorktreeProtection>) -> WorktreeAudit {
        WorktreeAudit {
            path: PathBuf::from(path),
            branch: "topic".to_owned(),
            short_object: "1234567".to_owned(),
            state: WorktreeState::Clean,
            protection,
            subject: "test".to_owned(),
            diff_base: "origin/main".to_owned(),
            diffstat: "1 file changed".to_owned(),
            bases: Vec::new(),
        }
    }

    #[test]
    fn never_selects_the_main_worktree() {
        let mut app = App::new(vec![worktree("/main", Some(WorktreeProtection::Main))]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.selected.is_empty());
    }

    #[test]
    fn requires_a_second_enter_before_removal() {
        let mut app = App::new(vec![worktree("/topic", None)]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Continue
        );
        assert!(app.confirming);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Remove
        );
    }

    #[test]
    fn force_removal_must_be_explicitly_toggled() {
        let mut app = App::new(vec![worktree("/topic", None)]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!app.force);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(app.force);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Remove
        );
    }
}
