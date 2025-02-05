use std::time::{Duration, Instant};

use procfs::process::Process;
use termion::event::Key;
use termion::raw::IntoRawMode;
use tui::{backend::{Backend, TermionBackend}, text::{Span, Spans}};
use tui::layout::{Constraint, Direction, Layout, Rect};
use tui::style::*;
use tui::terminal::{Frame, Terminal};
use tui::widgets::*;

// pub const ERROR_STYLE: Style = Style::default().fg(Color::Red).bg(Color::Reset);

mod util;
use util::*;
mod ui;
use ui::AppWidget;

use std::fmt::Debug;

#[cfg(feature = "backtrace")]
fn get_backtrace() -> impl Debug {
    backtrace::Backtrace::new()
}
#[cfg(not(feature = "backtrace"))]
fn get_backtrace() -> impl Debug {
    "Rebuild with the `backtrace` feature to enable backtraces on panic"
}

pub fn set_panic_handler() {
    use std::io::Write;

    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = get_backtrace();

        // log this panic to disk:
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .append(true)
            .create(true)
            .truncate(false)
            .open("panic.log")
        {
            let _ = writeln!(file, "======");
            let _ = writeln!(file, "Panic!");
            let payload = info.payload();
            if let Some(m) = payload.downcast_ref::<&str>() {
                let _ = writeln!(file, "{}", m);
            } else if let Some(m) = payload.downcast_ref::<String>() {
                let _ = writeln!(file, "{}", m);
            } else {
                let _ = writeln!(file, "{:?}", payload);
            }

            if let Some(loc) = info.location() {
                let _ = writeln!(file, "Location: {}", loc);
            }
            let _ = writeln!(file, "\n{:?}", bt);
        }
        old_hook(info)
    }));
}

struct TabState<'a> {
    pub labels: &'a [&'a str],
    current_idx: usize,
}

impl<'a> TabState<'a> {
    fn new(labels: &'a [&'a str]) -> TabState<'a> {
        TabState {
            labels,
            current_idx: 0,
        }
    }
    fn current(&self) -> usize {
        self.current_idx
    }
    fn current_label(&self) -> &'a str {
        &self.labels[self.current_idx]
    }
    fn select_next(&mut self) {
        self.current_idx = (self.current_idx + 1) % self.labels.len();
    }
    fn select_prev(&mut self) {
        if self.current_idx == 0 {
            self.current_idx = self.labels.len() - 1;
        } else {
            self.current_idx -= 1;
        }
    }
    fn select_by_char(&mut self, c: char) -> ui::InputResult {
        for (idx, label) in self.labels.iter().enumerate() {
            if label.chars().next() == Some(c) {
                self.current_idx = idx;
                return ui::InputResult::NeedsRedraw;
            }
        }
        ui::InputResult::None
    }
}

struct StatDelta<T> {
    proc: Process,
    tps: i64,
    old: T,
    old_when: Instant,
    new: T,
    new_when: Instant,
}

impl StatDelta<procfs::process::Io> {
    fn new(proc: Process) -> anyhow::Result<StatDelta<procfs::process::Io>> {
        let s = proc.io()?;
        let now = Instant::now();
        Ok(StatDelta {
            proc,
            old: s,
            new: s,
            old_when: now,
            new_when: now,
            tps: procfs::ticks_per_second().unwrap(),
        })
    }
    fn update(&mut self) {
        if let Ok(io) = self.proc.io() {
            std::mem::swap(&mut self.old, &mut self.new);
            self.old_when = self.new_when;
            self.new = io;
            self.new_when = Instant::now();
        }
    }
}

impl StatDelta<procfs::process::Stat> {
    fn new(proc: Process) -> StatDelta<procfs::process::Stat> {
        let s = proc.stat.clone();
        let now = Instant::now();
        StatDelta {
            proc,
            old: s.clone(),
            new: s,
            old_when: now,
            new_when: now,
            tps: procfs::ticks_per_second().unwrap(),
        }
    }
    fn update(&mut self) {
        if let Ok(new_stat) = self.proc.stat() {
            std::mem::swap(&mut self.old, &mut self.new);
            self.old_when = self.new_when;
            self.new = new_stat;
            self.new_when = Instant::now();
        }
    }
    fn cpu_percentage(&self) -> f32 {
        let d = self.duration();
        if d < Duration::from_millis(100) {
            return 0.0;
        }
        let cputime_delta = ((self.new.utime - self.old.utime) + (self.new.stime - self.old.stime))
            as f32
            / self.tps as f32;
        let usage = cputime_delta / (d.as_millis() as f32 / 1000.0);

        usage * 100.0
    }
}

impl<T> StatDelta<T> {
    fn latest(&self) -> &T {
        &self.new
    }
    fn previous(&self) -> &T {
        &self.old
    }
    fn duration(&self) -> Duration {
        self.new_when - self.old_when
    }
}

struct SparklineData {
    data: Vec<u64>,
    max_len: usize,
}

impl SparklineData {
    fn new() -> SparklineData {
        let max_len = 400;
        SparklineData {
            data: Vec::with_capacity(max_len),
            max_len,
        }
    }
    fn push(&mut self, val: u64) {
        self.data.push(val);
        if self.data.len() > self.max_len {
            self.data.remove(0);
        }
    }

    fn as_slice(&self) -> &[u64] {
        //let s = std::cmp::max(0, self.data.len() as i32 - num_elems as i32) as usize;
        self.data.as_slice()
    }
}

pub struct App<'a> {
    tps: i64,
    proc: Process,
    env_widget: ui::EnvWidget,
    net_widget: ui::NetWidget,
    maps_widget: ui::MapsWidget,
    files_widget: ui::FilesWidget,
    limit_widget: ui::LimitWidget,
    tree_widget: ui::TreeWidget,
    cgroup_widget: ui::CGroupWidget,
    io_widget: ui::IOWidget,
    task_widget: ui::TaskWidget,
    tab: TabState<'a>,
    stat_d: StatDelta<procfs::process::Stat>,
    cpu_spark: SparklineData,
}

impl<'a> App<'a> {
    fn new(proc: Process) -> App<'a> {
        App {
            env_widget: ui::EnvWidget::new(&proc),
            net_widget: ui::NetWidget::new(&proc),
            maps_widget: ui::MapsWidget::new(&proc),
            files_widget: ui::FilesWidget::new(&proc),
            limit_widget: ui::LimitWidget::new(&proc),
            tree_widget: ui::TreeWidget::new(&proc),
            cgroup_widget: ui::CGroupWidget::new(&proc),
            io_widget: ui::IOWidget::new(&proc),
            task_widget: ui::TaskWidget::new(&proc),
            tps: procfs::ticks_per_second().unwrap(),
            stat_d: StatDelta::<procfs::process::Stat>::new(proc.clone()),
            tab: TabState::new(&[
                ui::EnvWidget::TITLE,
                ui::NetWidget::TITLE,
                ui::MapsWidget::TITLE,
                ui::FilesWidget::TITLE,
                ui::LimitWidget::TITLE,
                ui::TreeWidget::TITLE,
                ui::CGroupWidget::TITLE,
                ui::IOWidget::TITLE,
                ui::TaskWidget::TITLE,
            ]),
            cpu_spark: SparklineData::new(),
            proc,
        }
    }

    fn switch_to(&mut self, new_pid: i32) {
        if let Ok(proc) = Process::new(new_pid) {
            self.env_widget = ui::EnvWidget::new(&proc);
            self.net_widget = ui::NetWidget::new(&proc);
            self.maps_widget = ui::MapsWidget::new(&proc);
            self.files_widget = ui::FilesWidget::new(&proc);
            self.limit_widget = ui::LimitWidget::new(&proc);
            self.tree_widget = ui::TreeWidget::new(&proc);
            self.cgroup_widget = ui::CGroupWidget::new(&proc);
            self.task_widget = ui::TaskWidget::new(&proc);
            self.io_widget = ui::IOWidget::new(&proc);
            self.stat_d = StatDelta::<procfs::process::Stat>::new(proc.clone());
            self.cpu_spark = SparklineData::new();
            self.proc = proc;
        }
    }

    fn handle_input(&mut self, input: Key, height: u16) -> ui::InputResult {
        let widget_redraw = match self.tab.current_label() {
            ui::EnvWidget::TITLE => self.env_widget.handle_input(input, height),
            ui::NetWidget::TITLE => self.net_widget.handle_input(input, height),
            ui::MapsWidget::TITLE => self.maps_widget.handle_input(input, height),
            ui::FilesWidget::TITLE => self.files_widget.handle_input(input, height),
            ui::LimitWidget::TITLE => self.limit_widget.handle_input(input, height),
            ui::CGroupWidget::TITLE => self.cgroup_widget.handle_input(input, height),
            ui::IOWidget::TITLE => self.io_widget.handle_input(input, height),
            ui::TaskWidget::TITLE => self.task_widget.handle_input(input, height),
            ui::TreeWidget::TITLE => {
                if input == Key::Char('\n') {
                    let new_pid = self.tree_widget.get_selected_pid();
                    if new_pid != self.proc.stat.pid {
                        self.switch_to(new_pid);
                        return ui::InputResult::NeedsUpdate;
                    }
                }
                self.tree_widget.handle_input(input, height)
            }
            _ => ui::InputResult::None,
        };
        let input_redraw = match input {
            Key::Char('\t') | Key::Right => {
                self.tab.select_next();
                ui::InputResult::NeedsRedraw
            }
            Key::BackTab | Key::Left => {
                self.tab.select_prev();
                ui::InputResult::NeedsRedraw
            }
            Key::Char(c) => self.tab.select_by_char(c),
            _ => ui::InputResult::None,
        };
        widget_redraw | input_redraw
    }

    fn tick(&mut self) {
        if self.proc.is_alive() {
            self.env_widget.update(&self.proc);
            self.net_widget.update(&self.proc);
            self.files_widget.update(&self.proc);
            self.limit_widget.update(&self.proc);
            self.tree_widget.update(&self.proc);
            self.cgroup_widget.update(&self.proc);
            self.io_widget.update(&self.proc);
            self.task_widget.update(&self.proc);
            self.stat_d.update();

            let cpu_usage = self.stat_d.cpu_percentage();
            self.cpu_spark.push(cpu_usage.round() as u64);
        }
    }

    fn draw_top<B: Backend>(&self, f: &mut Frame<B>, top_area: Rect, area: Rect) {
        // first first line is the pid and process name
        let mut text = Vec::new();
        if let Ok(cmdline) = self.proc.cmdline() {
            let mut i = cmdline.into_iter();
            if let Some(exe) = i.next() {
                text.push(Span::raw("\u{2500} "));
                text.push(Span::styled(exe, Style::default().fg(Color::Magenta)));
                text.push(Span::raw(" "));
            }
            for arg in i {
                text.push(Span::raw(arg));
                text.push(Span::raw(" "));
            }
        } else {
            text.push(Span::raw(format!("\u{2500} {} ", self.proc.stat.comm)));
        }

        text.push(Span::raw("\u{2500}".repeat(top_area.width as usize)));
        f.render_widget(Paragraph::new(Spans::from(text)), top_area);

        // top frame is composed of 3 horizontal blocks
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .margin(0)
            .constraints(
                [
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                    Constraint::Ratio(1, 3),
                ]
                .as_ref(),
            )
            .split(area);

        // first block is basic state info
        let s = Style::default().fg(Color::Green);
        let mut text: Vec<Spans> = Vec::new();
        
        // first line:
        // pid:19610 ppid:8959 pgrp:19610 session:8959
        text.push(Spans::from(vec![
            Span::styled("pid:", s),
            Span::raw(format!("{} ", self.proc.stat.pid)),
            Span::styled("ppid:", s),
            Span::raw(format!("{} ", self.proc.stat.ppid)),
            Span::styled("pgrp:", s),
            Span::raw(format!("{} ", self.proc.stat.pgrp)),
            Span::styled("session:", s),
            Span::raw(format!("{}", self.proc.stat.session))
        ]));

        // second line:
        // state:X (Dead) started:23:14:28
        text.push(Spans::from(vec![
            Span::styled("state:", s),
            if self.proc.is_alive() {
                Span::raw(format!(
                    "{} ({:?}) ",
                    self.proc.stat.state,
                    self.proc.stat.state().unwrap()
                ))
            } else {
                Span::raw(format!("X (Dead) "))
            },

            Span::styled("started:", s),
            if let Ok(dt) = self.proc.stat.starttime() {
                Span::raw(format!("{}\n", fmt_time(dt)))
            } else {
                Span::styled("(unknown)\n", Style::default().fg(Color::Red).bg(Color::Reset))
            }
        ]));

        // third line:
        // owner:achin(1000) threads:

        let status = self.proc.status();
        if let Ok(ref status) = status {
            text.push(Spans::from(vec![
                Span::styled("owner:", s),
                Span::raw(format!(
                    "{}({}) ",
                    lookup_username(status.ruid),
                    status.ruid
                )),
                Span::styled("threads:", s),
                Span::raw(format!("{}\n", status.threads))
            ]));
        }

        // forth line:
        // nice:0

        text.push(Spans::from(vec![
            Span::styled("nice:", s),
            Span::raw(format!("{} ", self.proc.stat.nice))
        ]));

        let widget = Paragraph::new(text)
            .block(Block::default().borders(Borders::RIGHT));
        f.render_widget(widget, chunks[0]);

        // second block is CPU time info

        let mut text: Vec<Spans> = Vec::new();
        let stat = self.stat_d.latest();
        let u_time = Duration::from_millis(stat.utime * (1000.0 / self.tps as f32) as u64);
        let s_time = Duration::from_millis(stat.stime * (1000.0 / self.tps as f32) as u64);

        let usage = self.stat_d.cpu_percentage();

        // first line:
        // cpu usage:0.00%
        text.push(Spans::from(vec![
            Span::styled("cpu usage:", s),
            Span::raw(format!("{:.2}%", usage))
        ]));

        // second line:
        // │user time:70ms kernel time:10ms u/k:87.50%
        let percent_user = stat.utime as f32 / (stat.utime + stat.stime) as f32;

        text.push(Spans::from(vec![

            Span::styled("user time:", s),
            Span::raw(format!("{:?} ", u_time)),
            Span::styled("kernel time:", s),
            Span::raw(format!("{:?} ", s_time)),
    
            // how much time is in userland
            Span::styled("u/k:", s),
            Span::raw(format!("{:.2}%\n", percent_user * 100.0)),
        ]));

        // third line:
        // virt:12.14 MB rss:5.55 MB shr:3.46 MB

        let mut line: Vec<Span> = Vec::new();
      

        if let Ok(ref status) = status {
            // get some memory stats
            if let Some(vmsize) = status.vmsize {
                line.push(Span::styled("virt:", s));
                line.push(Span::raw(format!("{} ", fmt_bytes(vmsize * 1024, "B"))));
            }
            if let Some(rss) = status.vmrss {
                line.push(Span::styled("rss:", s));
                line.push(Span::raw(format!("{} ", fmt_bytes(rss * 1024, "B"))));
            }
            if let (Some(shr), Some(rss)) = (status.rssshmem, status.rssfile) {
                line.push(Span::styled("shr:", s));
                line.push(Span::raw(format!(
                    "{} ",
                    fmt_bytes((shr + rss) * 1024, "B")
                )));
            }
        }
        text.push(Spans::from(line));

        let widget = Paragraph::new(text)
            .block(Block::default().borders(Borders::RIGHT));
        f.render_widget(widget, chunks[1]);

        // third block is ????
    }

    fn draw_tab_selector<B: Backend>(&self, f: &mut Frame<B>, area: Rect) {
        let titles = self.tab.labels.iter().cloned().map(Spans::from).collect();
        let widget = Tabs::new(titles)
            .block(Block::default().borders(Borders::TOP | Borders::BOTTOM))
            // .titles(self.tab.labels)
            .select(self.tab.current())
            .style(Style::default().fg(Color::Cyan))
            .highlight_style(Style::default().fg(Color::Yellow));
        f.render_widget(widget, area);
    }
    fn draw_tab_body<B: Backend>(&mut self, f: &mut Frame<B>, area: Rect) {
        // split this into the body and a scrollbar area
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .margin(0)
            .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
            .split(area);

        match self.tab.current_label() {
            ui::EnvWidget::TITLE => {
                self.env_widget.draw(f, chunks[0]);
                self.env_widget.draw_scrollbar(f, chunks[1]);
            }
            ui::NetWidget::TITLE => {
                self.net_widget.draw(f, chunks[0]);
                self.net_widget.draw_scrollbar(f, chunks[1]);
            }
            ui::MapsWidget::TITLE => {
                self.maps_widget.draw(f, chunks[0]);
                self.maps_widget.draw_scrollbar(f, chunks[1]);
            }
            ui::FilesWidget::TITLE => {
                self.files_widget.draw(f, chunks[0]);
                self.files_widget.draw_scrollbar(f, chunks[1]);
            }
            ui::LimitWidget::TITLE => {
                self.limit_widget.draw(f, area);
            }
            ui::TreeWidget::TITLE => {
                self.tree_widget.draw(f, area);
            }
            ui::CGroupWidget::TITLE => {
                self.cgroup_widget.draw(f, area);
            }
            ui::IOWidget::TITLE => {
                self.io_widget.draw(f, area);
            }
            ui::TaskWidget::TITLE => {
                self.task_widget.draw(f, area);
                self.task_widget.draw_scrollbar(f, chunks[1]);

            }
            t => {
                panic!("Unhandled tab {}", t);
            }
        }
    }
    fn draw_cpu_spark<B: Backend>(&mut self, f: &mut Frame<B>, area: Rect) {
        // cpu sparkline (how the last area.width datapoints)
        let data = self.cpu_spark.as_slice();
        let s = std::cmp::max(0, data.len() as i32 - area.width as i32) as usize;
        let widget = Sparkline::default()
            .block(
                Block::default()
                    .title("Cpu Usage:")
                    .borders(Borders::TOP | Borders::BOTTOM),
            )
            .data(&data[s..])
            .max(100);
        f.render_widget(widget, area);
    }
}

/// Dedicated input testing mode, to debug terminals that don't report key presses in an expected way
fn run_keyboard_input_test() -> Result<(), anyhow::Error> {
    use termion::event::Event as TEvent;
    use termion::input::TermRead;

    let stdout = std::io::stdout().into_raw_mode()?;
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    let stdin = std::io::stdin();

    for evt in stdin.events() {
        terminal.clear()?;
        println!("{:?}", evt);
        if let Ok(TEvent::Key(Key::Char('q'))) = evt {
            println!();
            break;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();

    if args.iter().any(|a| a == "--keytest") {
        return run_keyboard_input_test();
    }

    let pid = args.get(1).and_then(|s| i32::from_str_radix(&s, 10).ok());

    let prc = if let Some(pid) = pid {
        procfs::process::Process::new(pid).unwrap()
    } else {
        procfs::process::Process::myself().unwrap()
    };

    set_panic_handler();

    let events = util::Events::new();

    let stdout = std::io::stdout().into_raw_mode()?;
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    let mut app = App::new(prc);

    let mut need_redraw = true;
    let mut tab_body_height = 0;
    loop {
        if need_redraw {
            // vertical layout has 5 sections:
            terminal.draw(|mut f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(0)
                    .constraints(
                        [
                            Constraint::Length(1),     // very top line
                            Constraint::Length(4 + 2), // top fixed-sized info box
                            Constraint::Length(1 + 2), // tab selector
                            Constraint::Min(0),        // tab body
                            Constraint::Length(5),     // cpu sparkline
                            Constraint::Length(5),     // cpu sparkline
                        ]
                        .as_ref(),
                    )
                    .split(f.size());

                tab_body_height = chunks[3].height;

                app.draw_top(&mut f, chunks[0], chunks[1]);
                app.draw_tab_selector(&mut f, chunks[2]);
                app.draw_tab_body(&mut f, chunks[3]);
                app.draw_cpu_spark(&mut f, chunks[4]);
            })?;
            need_redraw = false;
        }

        match events.rx.recv() {
            Err(..) => break,
            Ok(Event::Key(Key::Esc))
            | Ok(Event::Key(Key::Char('q')))
            | Ok(Event::Key(Key::Ctrl('c'))) => break,

            Ok(Event::Key(k)) => match app.handle_input(k, tab_body_height) {
                ui::InputResult::NeedsUpdate => {
                    need_redraw = true;
                    app.tick();
                }
                ui::InputResult::NeedsRedraw => {
                    need_redraw = true;
                }
                _ => {}
            },
            Ok(Event::Tick) => {
                need_redraw = true;
                app.tick();
            }

            _ => {}
        }
    }

    //println!("\n-----");
    //println!("{:?}", prc);

    Ok(())
}
