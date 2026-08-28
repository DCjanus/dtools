use std::collections::BTreeSet;
use std::io::{self, Stderr};

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
use crate::git::{BaseAudit, BranchAudit, DeleteKind};

type Tui = Terminal<CrosstermBackend<Stderr>>;

pub(crate) fn select_branches(branches: Vec<BranchAudit>) -> AnyResult<Vec<String>> {
    let mut terminal = start_terminal()?;
    let _guard = TerminalGuard;
    let mut app = App::new(branches);

    loop {
        terminal
            .draw(|frame| render(frame, &app))
            .context("failed to draw branch selector")?;
        let Event::Key(key) = event::read().context("failed to read terminal input")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Outcome::Continue => {}
            Outcome::Cancel => return Ok(Vec::new()),
            Outcome::Delete => return Ok(app.selected.into_iter().collect()),
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
    Delete,
}

struct App {
    branches: Vec<BranchAudit>,
    selected: BTreeSet<String>,
    cursor: usize,
    gone_only: bool,
    confirming: bool,
}

impl App {
    fn new(branches: Vec<BranchAudit>) -> Self {
        Self {
            branches,
            selected: BTreeSet::new(),
            cursor: 0,
            gone_only: false,
            confirming: false,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.branches
            .iter()
            .enumerate()
            .filter_map(|(index, branch)| {
                (!self.gone_only || branch.tracking == "[gone]").then_some(index)
            })
            .collect()
    }

    fn current(&self) -> Option<&BranchAudit> {
        self.visible_indices()
            .get(self.cursor)
            .and_then(|index| self.branches.get(*index))
    }

    fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.confirming {
            return match key.code {
                KeyCode::Enter => Outcome::Delete,
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
                self.cursor = self.visible_indices().len().saturating_sub(1);
                Outcome::Continue
            }
            KeyCode::Char(' ') => {
                self.toggle_current();
                Outcome::Continue
            }
            KeyCode::Char('a') => {
                self.select_visible();
                Outcome::Continue
            }
            KeyCode::Char('x') => {
                self.selected.clear();
                Outcome::Continue
            }
            KeyCode::Char('g') => {
                self.gone_only = !self.gone_only;
                self.cursor = 0;
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
        let last = self.visible_indices().len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(amount).min(last);
    }

    fn toggle_current(&mut self) {
        let Some(branch) = self.current() else {
            return;
        };
        if branch.protected_reason.is_some() {
            return;
        }
        let name = branch.name.clone();
        if !self.selected.remove(&name) {
            self.selected.insert(name);
        }
    }

    fn select_visible(&mut self) {
        let selectable = self
            .visible_indices()
            .into_iter()
            .filter_map(|index| self.branches.get(index))
            .filter(|branch| branch.protected_reason.is_none())
            .map(|branch| branch.name.clone())
            .collect::<Vec<_>>();
        if selectable
            .iter()
            .all(|branch| self.selected.contains(branch))
        {
            for branch in selectable {
                self.selected.remove(&branch);
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
                        .title(" 本地分支清理 "),
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
    render_branch_list(frame, columns[0], app);
    render_audit(frame, columns[1], app);
    render_footer(frame, rows[2], app);

    if app.confirming {
        render_confirmation(frame, area, app);
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let visible = app.visible_indices().len();
    let filter = if app.gone_only {
        "仅跟踪分支已丢失"
    } else {
        "全部本地分支"
    };
    let title = Line::from(vec![
        Span::styled(
            " 本地分支清理 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {visible} 个分支  │  已选 {} 个  │  {filter}",
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

fn render_branch_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let indices = app.visible_indices();
    let items = indices
        .iter()
        .filter_map(|index| app.branches.get(*index))
        .map(|branch| {
            let (marker, marker_style) = if branch.protected_reason.is_some() {
                ("[-]", Style::default().fg(Color::DarkGray))
            } else if app.selected.contains(&branch.name) {
                ("[x]", Style::default().fg(Color::Green))
            } else {
                ("[ ]", Style::default().fg(Color::Gray))
            };
            let (upstream_badge, upstream_style) = upstream_badge(branch);
            let (audit_badge, audit_style) = audit_badge(branch);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), marker_style),
                Span::styled(upstream_badge, upstream_style),
                Span::styled(audit_badge, audit_style),
                Span::raw(&branch.name),
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
                .title(" 分支 · 跟踪  审计    名称 ")
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

fn upstream_badge(branch: &BranchAudit) -> (&'static str, Style) {
    if branch.tracking == "[gone]" {
        ("丢失  ", Style::default().fg(Color::LightRed))
    } else if branch.upstream.is_empty() {
        ("未设  ", Style::default().fg(Color::Yellow))
    } else {
        ("正常  ", Style::default().fg(Color::Cyan))
    }
}

fn audit_badge(branch: &BranchAudit) -> (&'static str, Style) {
    if branch.protected_reason.is_some() {
        return ("锁定    ", Style::default().fg(Color::DarkGray));
    }
    if branch
        .bases
        .iter()
        .any(|base| base.absorption == Some(DeleteKind::Merged))
    {
        return ("已合并  ", Style::default().fg(Color::Green));
    }
    if branch
        .bases
        .iter()
        .any(|base| base.absorption == Some(DeleteKind::Equivalent))
    {
        return ("等价    ", Style::default().fg(Color::LightGreen));
    }
    ("待复核  ", Style::default().fg(Color::LightRed))
}

fn render_audit(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(branch) = app.current() else {
        frame.render_widget(
            Paragraph::new("没有符合当前筛选条件的分支\n按 g 显示全部本地分支")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title(" 审计 ")),
            area,
        );
        return;
    };

    let upstream = if branch.upstream.is_empty() {
        "未设置"
    } else {
        &branch.upstream
    };
    let (tracking, tracking_color) = if branch.tracking == "[gone]" {
        ("丢失 — 远端跟踪分支已不存在", Color::LightRed)
    } else if branch.upstream.is_empty() {
        ("未设置 — 没有配置 upstream", Color::Yellow)
    } else {
        ("正常 — 远端跟踪分支存在", Color::Cyan)
    };
    let protection = protection_text(branch.protected_reason.as_deref());
    let mut lines = vec![
        audit_line("分支", &branch.name, Color::White),
        audit_line("提交", &branch.short_object, Color::Magenta),
        audit_line("上游", upstream, upstream_color(branch)),
        audit_line("跟踪", tracking, tracking_color),
        audit_line("保护", protection, protection_color(branch)),
        audit_line("作者", &branch.author, Color::White),
        audit_line("提交时间", &branch.committed_at, Color::White),
        audit_line("主题", &branch.subject, Color::White),
        Line::raw(""),
        Line::styled(
            "基准分支对比",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if branch.bases.is_empty() {
        lines.push(Line::styled(
            "  未检测到远端默认基准分支",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        lines.extend(branch.bases.iter().map(base_line));
    }
    lines.extend([
        Line::styled(
            format!("相对 {} 的差异", branch.diff_base),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("  {}", branch.diffstat)),
    ]);
    if !branch.worktree.is_empty() {
        lines.push(audit_line("工作树", &branch.worktree, Color::Yellow));
    }
    if area.height >= 24 {
        lines.extend([
            Line::raw(""),
            Line::styled(
                "“待复核”表示 dcx 无法确认改动已被吸收，请检查后再选择。",
                Style::default().fg(Color::LightRed),
            ),
        ]);
    }
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

fn protection_text(reason: Option<&str>) -> &'static str {
    match reason {
        Some("base branch") => "锁定 — 基准分支",
        Some("checked out in a worktree") => "锁定 — 正在工作树中使用",
        Some("excluded by rule") => "锁定 — 匹配排除规则",
        Some(_) => "锁定 — 受保护分支",
        None => "无 — 可以选择",
    }
}

fn upstream_color(branch: &BranchAudit) -> Color {
    if branch.tracking == "[gone]" {
        Color::LightRed
    } else if branch.upstream.is_empty() {
        Color::Yellow
    } else {
        Color::Cyan
    }
}

fn protection_color(branch: &BranchAudit) -> Color {
    if branch.protected_reason.is_some() {
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
        Span::styled(" g ", key_style()),
        Span::raw("筛选  "),
        Span::styled(" a ", key_style()),
        Span::raw("全选  "),
        Span::styled(" x ", key_style()),
        Span::raw("清空  "),
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
    let popup = centered_rect(68, 60, area);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::styled(
            format!(
                "要使用 git branch -D 删除这 {} 个本地分支吗？",
                app.selected.len()
            ),
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let limit = popup.height.saturating_sub(8) as usize;
    for branch in app.selected.iter().take(limit) {
        lines.push(Line::raw(format!("  • {branch}")));
    }
    let remaining = app.selected.len().saturating_sub(limit);
    if remaining > 0 {
        lines.push(Line::styled(
            format!("  … 还有 {remaining} 个分支"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.extend([
        Line::raw(""),
        Line::styled(
            "此操作会强制删除选中的本地引用，不会修改远端分支。",
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

    fn branch(name: &str, tracking: &str, protected: bool) -> BranchAudit {
        BranchAudit {
            name: name.to_owned(),
            short_object: "1234567".to_owned(),
            upstream: format!("origin/{name}"),
            tracking: tracking.to_owned(),
            worktree: String::new(),
            protected_reason: protected.then(|| "base branch".to_owned()),
            author: "Test User".to_owned(),
            committed_at: "2026-08-12T00:00:00+08:00".to_owned(),
            subject: "test".to_owned(),
            diff_base: "origin/main".to_owned(),
            diffstat: "1 file changed".to_owned(),
            bases: Vec::new(),
        }
    }

    #[test]
    fn defaults_to_all_branches_and_can_filter_to_gone() {
        let mut app = App::new(vec![
            branch("gone", "[gone]", false),
            branch("live", "", false),
        ]);

        assert_eq!(app.visible_indices(), vec![0, 1]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.selected.contains("gone"));

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.visible_indices(), vec![0]);
    }

    #[test]
    fn never_selects_protected_branches() {
        let mut app = App::new(vec![branch("main", "[gone]", true)]);

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        assert!(app.selected.is_empty());
    }

    #[test]
    fn requires_a_second_enter_before_deletion() {
        let mut app = App::new(vec![branch("gone", "[gone]", false)]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Continue
        );
        assert!(app.confirming);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Outcome::Delete
        );
    }
}
