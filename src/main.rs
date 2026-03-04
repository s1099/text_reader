use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, IconName, Root, Sizable};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod viewer;
use viewer::{get_lines, MappedFile};

// ─── Tab state ────────────────────────────────────────────────────────────────

enum TabContent {
    Welcome,
    File(MappedFile),
    Error(String),
}

struct TabEntry {
    title: String,
    content: TabContent,
    /// Scroll handle owned by this tab; passed to `uniform_list` each frame.
    scroll_handle: UniformListScrollHandle,
    /// Set to true once after a file opens so render can auto-focus the viewport.
    needs_focus: bool,
}

// ─── Main view ────────────────────────────────────────────────────────────────

struct TextEditor {
    focus_handle: FocusHandle,
    tabs: Vec<TabEntry>,
    active_tab: usize,
    /// Background tasks must be held alive (dropping a Task cancels it).
    _tasks: Vec<Task<()>>,
}

impl TextEditor {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            tabs: vec![TabEntry {
                title: "Welcome".to_string(),
                content: TabContent::Welcome,
                scroll_handle: UniformListScrollHandle::new(),
                needs_focus: false,
            }],
            active_tab: 0,
            _tasks: vec![],
        }
    }

    // ── Scroll helpers ────────────────────────────────────────────────────────

    /// Scroll the active file tab by `delta` lines (positive = down).
    fn scroll_lines(&mut self, delta: i64, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let (current_top, total) = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::File(mf) => {
                    let top = tab.scroll_handle.0.borrow().base_handle.top_item() as i64;
                    let total = mf.total_lines() as i64;
                    (top, total)
                }
                _ => return,
            }
        };
        let new_top = (current_top + delta).max(0).min(total.saturating_sub(1)) as usize;
        self.tabs[active]
            .scroll_handle
            .scroll_to_item_strict(new_top, ScrollStrategy::Top);
        cx.notify();
    }

    fn scroll_to_start(&mut self, cx: &mut Context<Self>) {
        let active = self.active_tab;
        if matches!(self.tabs[active].content, TabContent::File(_)) {
            self.tabs[active]
                .scroll_handle
                .scroll_to_item_strict(0, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        let active = self.active_tab;
        let total = match &self.tabs[active].content {
            TabContent::File(mf) => mf.total_lines() as usize,
            _ => return,
        };
        self.tabs[active]
            .scroll_handle
            .scroll_to_item_strict(total.saturating_sub(1), ScrollStrategy::Bottom);
        cx.notify();
    }

    // ── File opening ──────────────────────────────────────────────────────────

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        // Switch to existing tab if this file is already open.
        if let Some(idx) = self.tabs.iter().position(|t| t.title == title) {
            self.active_tab = idx;
            cx.notify();
            return;
        }

        match MappedFile::open(path) {
            Ok(mf) => {
                mf.start_indexing();

                // Spawn a periodic task that calls cx.notify() until indexing
                // finishes, so the status bar line count updates live.
                let index_arc = Arc::clone(&mf.index);
                let task = cx.spawn(async move |weak, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                        let complete =
                            index_arc.read().map(|i| i.complete).unwrap_or(true);
                        let Some(entity) = weak.upgrade() else { break };
                        if cx.update_entity(&entity, |_, ctx| ctx.notify()).is_err()
                            || complete
                        {
                            break;
                        }
                    }
                });
                self._tasks.push(task);

                self.tabs.push(TabEntry {
                    title,
                    content: TabContent::File(mf),
                    scroll_handle: UniformListScrollHandle::new(),
                    needs_focus: true,
                });
                self.active_tab = self.tabs.len() - 1;
            }
            Err(e) => {
                self.tabs.push(TabEntry {
                    title,
                    content: TabContent::Error(e.to_string()),
                    scroll_handle: UniformListScrollHandle::new(),
                    needs_focus: false,
                });
                self.active_tab = self.tabs.len() - 1;
            }
        }
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.len() > 1 {
            self.tabs.remove(index);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            cx.notify();
        }
    }
}

// ─── Render ───────────────────────────────────────────────────────────────────

impl Render for TextEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_tab;

        // Auto-focus the viewport when a new file tab is first rendered.
        if self.tabs.get(active).map(|t| t.needs_focus).unwrap_or(false) {
            if let Some(tab) = self.tabs.get_mut(active) {
                tab.needs_focus = false;
            }
            self.focus_handle.focus(window);
        }

        let weak = cx.weak_entity();

        // ── File menu ─────────────────────────────────────────────────────────
        let weak_new = weak.clone();
        let weak_open = weak.clone();

        let file_menu = Button::new("file-menu-btn")
            .label("File")
            .ghost()
            .dropdown_menu(move |menu, _, _| {
                let w_new = weak_new.clone();
                let w_open = weak_open.clone();
                menu.item(
                    PopupMenuItem::new("New Tab")
                        .icon(IconName::File)
                        .on_click(move |_, _, cx| {
                            w_new
                                .update(cx, |view, cx| {
                                    let idx = view.tabs.len() + 1;
                                    view.tabs.push(TabEntry {
                                        title: format!("Untitled {idx}"),
                                        content: TabContent::Welcome,
                                        scroll_handle: UniformListScrollHandle::new(),
                                        needs_focus: false,
                                    });
                                    view.active_tab = view.tabs.len() - 1;
                                    cx.notify();
                                })
                                .ok();
                        }),
                )
                .item(
                    PopupMenuItem::new("Open File...")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            let picked = rfd::FileDialog::new()
                                .add_filter("All Files", &["*"])
                                .pick_file();
                            if let Some(path) = picked {
                                w_open
                                    .update(cx, |view, cx| {
                                        view.open_file(path, cx);
                                    })
                                    .ok();
                            }
                        }),
                )
                .separator()
                .item(PopupMenuItem::new("Exit").on_click(|_, _, cx| cx.quit()))
            });

        // ── Edit / View / Help menus ──────────────────────────────────────────
        let edit_menu = Button::new("edit-menu-btn")
            .label("Edit")
            .ghost()
            .dropdown_menu(|menu, _, _| {
                menu.item(PopupMenuItem::new("Copy").icon(IconName::Copy).disabled(true))
                    .separator()
                    .item(
                        PopupMenuItem::new("Find")
                            .icon(IconName::Search)
                            .disabled(true),
                    )
            });

        let view_menu = Button::new("view-menu-btn")
            .label("View")
            .ghost()
            .dropdown_menu(|menu, _, _| {
                menu.item(
                    PopupMenuItem::new("Zoom In")
                        .icon(IconName::Plus)
                        .disabled(true),
                )
                .item(
                    PopupMenuItem::new("Zoom Out")
                        .icon(IconName::Minus)
                        .disabled(true),
                )
            });

        let help_menu = Button::new("help-menu-btn")
            .label("Help")
            .ghost()
            .dropdown_menu(|menu, _, _| {
                menu.item(
                    PopupMenuItem::new("About")
                        .icon(IconName::Info)
                        .disabled(true),
                )
            });

        // ── Menu bar ──────────────────────────────────────────────────────────
        let menu_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .px_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(file_menu)
            .child(edit_menu)
            .child(view_menu)
            .child(help_menu);

        // ── Tab bar ───────────────────────────────────────────────────────────
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                Tab::new()
                    .label(tab.title.clone())
                    .suffix(
                        Button::new(ElementId::Integer(i as u64 + 1000))
                            .ghost()
                            .xsmall()
                            .label("×")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.close_tab(i, cx);
                            })),
                    )
            })
            .collect();

        let tab_bar = TabBar::new("main-tabs")
            .underline()
            .selected_index(active)
            .on_click(cx.listener(|this, ix: &usize, _window, cx| {
                this.active_tab = *ix;
                cx.notify();
            }))
            .children(tabs);

        // ── Content area ──────────────────────────────────────────────────────
        let content: AnyElement = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::Welcome => div()
                    .flex_1()
                    .p_8()
                    .font_family("monospace")
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(
                        div()
                            .mb_4()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child("File Viewer"),
                    )
                    .child(
                        div()
                            .child("Open file with File → Open File…")
                            .mb_2(),
                    )
                    .child(div().child("• Files of any size are supported via memory-mapping").mb_1())
                    .child(div().child("• A sparse line index is built in the background").mb_1())
                    .child(div().child("• Only the visible lines are ever decoded").mb_1())
                    .child(div().child("• Arrow / Page-Up/Dn / Home / End to navigate (broken)").mb_1())
                    .into_any(),

                TabContent::Error(msg) => {
                    let msg = msg.clone();
                    div()
                        .flex_1()
                        .p_6()
                        .font_family("monospace")
                        .text_sm()
                        .child(format!("Error opening file: {msg}"))
                        .into_any()
                }

                TabContent::File(mf) => {
                    let total_lines = mf.total_lines() as usize;
                    let mmap_arc = Arc::clone(&mf.mmap);
                    let index_arc = Arc::clone(&mf.index);
                    let scroll_handle = tab.scroll_handle.clone();
                    let scroll_handle_bar = scroll_handle.clone();

                    // Gutter color: same foreground at reduced alpha.
                    let mut gutter_color = cx.theme().foreground;
                    gutter_color.a *= 0.4;


                    let num_digits = total_lines.max(1).to_string().len().max(2);
                    let gutter_width = px(num_digits as f32 * 8.5 + 10.0);

                    div()
                        .id("content-area")
                        .flex_1()
                        .overflow_hidden()
                        .relative()
                        .track_focus(&self.focus_handle)
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                            match event.keystroke.key.as_str() {
                                "up" => this.scroll_lines(-1, cx),
                                "down" => this.scroll_lines(1, cx),
                                "pageup" => this.scroll_lines(-50, cx),
                                "pagedown" => this.scroll_lines(50, cx),
                                "home" => this.scroll_to_start(cx),
                                "end" => this.scroll_to_end(cx),
                                _ => {}
                            }
                        }))
                        .child(
                            uniform_list(
                                "file-content",
                                total_lines.max(1),
                                move |range, _window, _cx| {
                                    let lines = get_lines(
                                        &mmap_arc,
                                        &index_arc,
                                        range.start as u64,
                                        range.len(),
                                    );
                                    lines
                                        .into_iter()
                                        .enumerate()
                                        .map(|(i, line)| {
                                            let ln = range.start + i + 1;
                                            div()
                                                .flex()
                                                .flex_row()
                                                .font_family("monospace")
                                                .text_sm()
                                                .whitespace_nowrap()
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .w(gutter_width)
                                                        .pr_3()
                                                        .text_right()
                                                        .text_color(gutter_color)
                                                        .child(format!("{ln}")),
                                                )
                                                .child(div().child(line))
                                        })
                                        .collect()
                                },
                            )
                            .track_scroll(scroll_handle)
                            .size_full(),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .child(Scrollbar::new(&scroll_handle_bar)),
                        )
                        .into_any()
                }
            }
        };

        // ── Status bar ────────────────────────────────────────────────────────
        let status_text: String = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::File(mf) => {
                    let top_line =
                        tab.scroll_handle.0.borrow().base_handle.top_item() + 1;
                    let total = mf.total_lines();
                    let size_str = format_file_size(mf.file_size());

                    let path_str = mf.path.to_string_lossy();
                    if mf.is_indexed() {
                        format!(
                            "{}  │  Ln {}  /  {}  │  {}",
                            path_str,
                            format_number(top_line as u64),
                            format_number(total),
                            size_str
                        )
                    } else {
                        let pct = (mf.index_progress() * 100.0) as u32;
                        format!(
                            "{}  │  Ln {}  /  ~{}  │  Indexing {}%  │  {}",
                            path_str,
                            format_number(top_line as u64),
                            format_number(total),
                            pct,
                            size_str
                        )
                    }
                }
                TabContent::Welcome => "File Viewer | open a file to begin".to_string(),
                TabContent::Error(_) => "Error".to_string(),
            }
        };

        let status_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(22.0))
            .px_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .font_family("monospace")
            .text_xs()
            .text_color(cx.theme().foreground)
            .child(status_text);

        // ── Root layout ───────────────────────────────────────────────────────
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(menu_bar)
            .child(tab_bar)
            .child(content)
            .child(status_bar)
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() {
    let app = Application::new();

    app.run(move |cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(200.0), px(100.0)),
                    size: size(px(1280.0), px(800.0)),
                })),
                ..WindowOptions::default()
            };

            cx.open_window(window_options, |window, cx| {
                let view = cx.new(|cx| TextEditor::new(cx));
                cx.new(|cx| Root::new(view, window, cx))
            })?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

// ─── Formatting helpers ────────────────────────────────────────────────────────

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    }
}
