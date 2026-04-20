use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Root, Selectable, Sizable};
use gpui_component_assets::Assets;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

mod find;
mod viewer;
use find::{FindState, Match, search_in_mmap, slice_for_line_range};
use viewer::{MappedFile, get_lines};

// ─── Actions ──────────────────────────────────────────────────────────────────

actions!(text_editor, [Find, FindClose, FindPrev]);

// ─── Tab state ────────────────────────────────────────────────────────────────

enum TabContent {
    Scratch,
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
    /// Find bar; `None` when hidden. Bound to whatever tab was active when opened.
    find: Option<FindState>,
    /// Background tasks must be held alive (dropping a Task cancels it).
    _tasks: Vec<Task<()>>,
}

impl TextEditor {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            tabs: vec![TabEntry {
                title: "Untitled".to_string(),
                content: TabContent::Scratch,
                scroll_handle: UniformListScrollHandle::new(),
                needs_focus: true,
            }],
            active_tab: 0,
            find: None,
            _tasks: vec![],
        }
    }

    fn new_scratch_tab(&mut self, cx: &mut Context<Self>) {
        let n = self
            .tabs
            .iter()
            .filter(|t| matches!(t.content, TabContent::Scratch))
            .count()
            + 1;
        let title = if n == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {n}")
        };
        self.tabs.push(TabEntry {
            title,
            content: TabContent::Scratch,
            scroll_handle: UniformListScrollHandle::new(),
            needs_focus: true,
        });
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
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
                        let complete = index_arc.read().map(|i| i.complete).unwrap_or(true);
                        let Some(entity) = weak.upgrade() else { break };
                        if cx.update_entity(&entity, |_, ctx| ctx.notify()).is_err() || complete {
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
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(TabEntry {
                title: "Untitled".to_string(),
                content: TabContent::Scratch,
                scroll_handle: UniformListScrollHandle::new(),
                needs_focus: true,
            });
            self.active_tab = 0;
        } else if self.active_tab == index {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        } else if self.active_tab > index {
            self.active_tab -= 1;
        }
        // Close find when its owning tab disappears.
        if let Some(find) = &self.find {
            if find.tab_index >= self.tabs.len()
                || !matches!(self.tabs[find.tab_index].content, TabContent::File(_))
            {
                self.find = None;
            }
        }
        cx.notify();
    }

    // ── Find feature ──────────────────────────────────────────────────────────

    /// Ctrl+F: toggle the find bar for the active tab.
    ///
    /// If already open and bound to this tab, just refocus the input and
    /// select its contents (matches VS Code's behavior on repeated Ctrl+F).
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_tab;
        if !matches!(
            self.tabs.get(active).map(|t| &t.content),
            Some(TabContent::File(_))
        ) {
            return;
        }

        if let Some(find) = self.find.as_mut() {
            if find.tab_index == active {
                let input = find.query_input.clone();
                input.update(cx, |state, cx| {
                    state.focus(window, cx);
                });
                return;
            }
        }

        let query_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Find")
                .default_value("")
        });

        let subscription = cx.subscribe_in(
            &query_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.kick_off_search(cx);
                }
                InputEvent::PressEnter { .. } => {
                    this.find_next(window, cx);
                }
                _ => {}
            },
        );

        query_input.update(cx, |state, cx| state.focus(window, cx));

        self.find = Some(FindState {
            query_input,
            case_sensitive: false,
            matches: Arc::new(RwLock::new(Vec::new())),
            current: 0,
            tab_index: active,
            last_query: String::new(),
            search_gen: 0,
            pending_scroll: false,
            _subscription: Some(subscription),
            _search_task: None,
        });

        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut find) = self.find.take() {
            // Actively release the matches allocation. The `Arc` can still be
            // held by the previous frame's `uniform_list` closure and by any
            // in-flight background search, so just dropping `FindState` would
            // keep up to ~24 MB (MAX_MATCHES × size_of::<Match>) resident
            // until those clones go away. Replacing the inner `Vec` with an
            // empty one frees that capacity immediately.
            if let Ok(mut w) = find.matches.write() {
                *w = Vec::new();
            }
            // Bump the generation so any racing background search that's
            // already produced a result refuses to write it back.
            find.search_gen = find.search_gen.wrapping_add(1);
            // Cancel the in-flight task now (Drop cancels it); doing this
            // before `find` itself drops makes the intent explicit.
            find._search_task = None;
            drop(find);
            // Return focus to the file viewport so keyboard scrolling works again.
            self.focus_handle.focus(window);
            cx.notify();
        }
    }

    /// Spawn a background search for the current query/case flag. Previous
    /// task (if any) is dropped, and stale completions are filtered using
    /// `search_gen`.
    fn kick_off_search(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let active = find.tab_index;
        let mmap = match self.tabs.get(active).map(|t| &t.content) {
            Some(TabContent::File(mf)) => Arc::clone(&mf.mmap),
            _ => return,
        };
        let query = find.query_input.read(cx).value().to_string();
        let case_insensitive = !find.case_sensitive;

        if query == find.last_query {
            return;
        }
        find.last_query = query.clone();
        find.search_gen += 1;
        let current_gen = find.search_gen;
        let results_arc = Arc::clone(&find.matches);

        if query.is_empty() {
            // Replace (don't `clear`) so we return the backing allocation to
            // the allocator instead of keeping ~24 MB of capacity around.
            if let Ok(mut w) = results_arc.write() {
                *w = Vec::new();
            }
            find.current = 0;
            find._search_task = None;
            cx.notify();
            return;
        }

        let task = cx.spawn(async move |weak, cx| {
            let results = cx
                .background_executor()
                .spawn(async move { search_in_mmap(&mmap, &query, case_insensitive) })
                .await;

            let Some(entity) = weak.upgrade() else { return };
            let _ = cx.update_entity(&entity, |this, cx| {
                let Some(find) = this.find.as_mut() else {
                    // Find was closed while we were searching; let `results`
                    // drop here so the large vector is freed promptly.
                    return;
                };
                if find.search_gen != current_gen {
                    return; // a newer search started while we were working
                }
                if let Ok(mut w) = results_arc.write() {
                    *w = results;
                }
                find.current = 0;
                find.pending_scroll = true;
                find._search_task = None;
                cx.notify();
            });
        });
        find._search_task = Some(task);
    }

    fn find_next(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let n = find.match_count();
        if n == 0 {
            return;
        }
        find.current = (find.current + 1) % n;
        find.pending_scroll = true;
        cx.notify();
    }

    fn find_prev(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let n = find.match_count();
        if n == 0 {
            return;
        }
        find.current = if find.current == 0 {
            n - 1
        } else {
            find.current - 1
        };
        find.pending_scroll = true;
        cx.notify();
    }

    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        find.case_sensitive = !find.case_sensitive;
        // Force re-search with new casing flag by clearing last_query.
        find.last_query.clear();
        self.kick_off_search(cx);
        cx.notify();
    }

    /// VS Code-style find bar, rendered as an absolutely positioned overlay
    /// anchored to the top-right of the file viewport.
    fn render_find_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let find = self
            .find
            .as_ref()
            .expect("render_find_bar called without an open find state");

        let has_matches = find.match_count() > 0;
        let label = find.label();
        let case_on = find.case_sensitive;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let icon_color = |enabled: bool| if enabled { fg } else { muted };

        div()
            .absolute()
            .top(px(6.0))
            .right(px(20.0))
            .occlude()
            .key_context("FindBar")
            .on_action(cx.listener(|this, _: &FindClose, window, cx| {
                this.close_find(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FindPrev, window, cx| {
                this.find_prev(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py(px(2.0))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .shadow_md()
                    .child(
                        div().w(px(240.0)).child(
                            Input::new(&find.query_input)
                                .border_0()
                                .xsmall()
                                .focus_bordered(false)
                                .suffix(
                                    Button::new("find-case")
                                        .icon(
                                            Icon::new(IconName::CaseSensitive)
                                                .text_color(icon_color(case_on)),
                                        )
                                        .xsmall()
                                        .compact()
                                        .ghost()
                                        .selected(case_on)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_find_case(cx);
                                        })),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .min_w(px(70.0))
                            .px_1()
                            .text_sm()
                            .text_color(if has_matches { fg } else { muted })
                            .child(label),
                    )
                    .child(
                        Button::new("find-prev")
                            .icon(
                                Icon::new(IconName::ChevronUp).text_color(icon_color(has_matches)),
                            )
                            .xsmall()
                            .ghost()
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_prev(window, cx);
                            })),
                    )
                    .child(
                        Button::new("find-next")
                            .icon(
                                Icon::new(IconName::ChevronDown)
                                    .text_color(icon_color(has_matches)),
                            )
                            .xsmall()
                            .ghost()
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_next(window, cx);
                            })),
                    )
                    .child(
                        Button::new("find-close")
                            .icon(Icon::new(IconName::Close).text_color(fg))
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_find(window, cx);
                            })),
                    ),
            )
    }
}

/// Split `line` by the matches falling on it, emitting a flat row of spans
/// where matches are painted with `match_bg` (or `active_bg` for the active
/// match). Byte offsets that don't land on UTF-8 boundaries or are past the
/// truncated line length are skipped, still keeping the counter accurate.
fn render_line_with_matches(
    line: &str,
    matches_on_line: &[Match],
    base_ix: usize,
    active_ix: Option<usize>,
    match_bg: Hsla,
    active_bg: Hsla,
    active_fg: Hsla,
) -> AnyElement {
    let mut row = div().flex().flex_row();
    let mut cursor: usize = 0;
    let bytes_len = line.len();

    for (offset_in_group, m) in matches_on_line.iter().enumerate() {
        let start = m.col;
        let end = m.col + m.len;
        if start >= bytes_len {
            break;
        }
        let end = end.min(bytes_len);
        if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            continue;
        }
        if start > cursor {
            let pre = &line[cursor..start];
            row = row.child(div().child(pre.to_string()));
        } else if start < cursor {
            continue;
        }
        let slice = &line[start..end];
        let is_active = active_ix == Some(base_ix + offset_in_group);
        row = if is_active {
            row.child(
                div()
                    .bg(active_bg)
                    .text_color(active_fg)
                    .child(slice.to_string()),
            )
        } else {
            row.child(div().bg(match_bg).child(slice.to_string()))
        };
        cursor = end;
    }
    if cursor < bytes_len {
        row = row.child(div().child(line[cursor..].to_string()));
    }
    row.into_any_element()
}

// ─── Render ───────────────────────────────────────────────────────────────────

impl Render for TextEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
        let active = self.active_tab;

        // Auto-focus the viewport when a new file tab is first rendered.
        if self
            .tabs
            .get(active)
            .map(|t| t.needs_focus)
            .unwrap_or(false)
        {
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
                menu.item(PopupMenuItem::new("New Tab").icon(IconName::File).on_click(
                    move |_, _, cx| {
                        w_new
                            .update(cx, |view, cx| {
                                view.new_scratch_tab(cx);
                            })
                            .ok();
                    },
                ))
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
        let weak_find = weak.clone();
        let edit_menu = Button::new("edit-menu-btn")
            .label("Edit")
            .ghost()
            .dropdown_menu(move |menu, _, _| {
                let w_find = weak_find.clone();
                menu.item(
                    PopupMenuItem::new("Copy")
                        .icon(IconName::Copy)
                        .disabled(true),
                )
                .separator()
                .item(PopupMenuItem::new("Find").icon(IconName::Search).on_click(
                    move |_, window, cx| {
                        w_find
                            .update(cx, |view, cx| view.open_find(window, cx))
                            .ok();
                    },
                ))
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
            .gap_1()
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
                Tab::new().label(tab.title.clone()).suffix(
                    Button::new(ElementId::Integer(i as u64 + 1000))
                        .ghost()
                        .xsmall()
                        .label("×")
                        // TODO: Add icon, .icon isn't visible for some reason
                        // .icon(
                        //     Icon::new(IconName::Close)
                        //         .text_color(cx.theme().primary)
                        // )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.close_tab(i, cx);
                        })),
                )
            })
            .collect();

        let tab_bar = TabBar::new("main-tabs")
            .selected_index(active)
            .on_click(cx.listener(|this, ix: &usize, _window, cx| {
                if *ix < this.tabs.len() {
                    this.active_tab = *ix;
                    cx.notify();
                }
            }))
            .children(tabs);

        // ── Content area ──────────────────────────────────────────────────────
        let content: AnyElement = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::Scratch => {
                    let mut gutter_color = cx.theme().foreground;
                    gutter_color.a *= 0.4;
                    let gutter_width = px(2.0 * 8.5 + 10.0);

                    div()
                        .flex_1()
                        .overflow_hidden()
                        .track_focus(&self.focus_handle)
                        .bg(cx.theme().background)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .font_family("monospace")
                                .text_sm()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .w(gutter_width)
                                        .pr_3()
                                        .text_right()
                                        .text_color(gutter_color)
                                        .child("1"),
                                )
                                .child(div()),
                        )
                        .into_any()
                }

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

                    // ── Find integration ──────────────────────────────────────
                    //
                    // Extract everything we need from `self.find` up-front (as
                    // a plain tuple of owned/cloned values) so that the later
                    // `self.find.as_mut()` doesn't conflict with a still-live
                    // immutable borrow.
                    let (find_open, matches_arc, active_match_line, current_match_ix): (
                        bool,
                        Option<Arc<RwLock<Vec<Match>>>>,
                        Option<u64>,
                        Option<usize>,
                    ) = match self.find.as_ref() {
                        Some(find) if find.tab_index == active => {
                            let m = Arc::clone(&find.matches);
                            let active_line = m
                                .read()
                                .ok()
                                .and_then(|r| r.get(find.current).map(|mm| mm.line));
                            (true, Some(m), active_line, Some(find.current))
                        }
                        _ => (false, None, None, None),
                    };

                    // Scroll to the active match when a navigation just happened.
                    if let Some(find) = self.find.as_mut() {
                        if find.tab_index == active && find.pending_scroll {
                            find.pending_scroll = false;
                            if let Some(line) = active_match_line {
                                // A small offset keeps the highlighted line off
                                // the very top edge of the viewport.
                                self.tabs[active].scroll_handle.scroll_to_item_strict(
                                    line.saturating_sub(3) as usize,
                                    ScrollStrategy::Top,
                                );
                            }
                        }
                    }

                    // Highlight colors (hard-coded to look correct on both
                    // themes; theme.warning is too dim on some themes).
                    let match_bg = rgba(0x5a_4a_00_80); // muted amber
                    let active_bg = rgba(0xff_9e_00_cc); // strong amber
                    let active_fg = rgba(0x00_00_00_ff);

                    let content_view = div()
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

                                    // Pull out just the matches that fall in
                                    // the rendered line range; binary-searched
                                    // once per render, then partitioned per line.
                                    let all_matches_opt =
                                        matches_arc.as_ref().and_then(|a| a.read().ok());

                                    lines
                                        .into_iter()
                                        .enumerate()
                                        .map(|(i, line)| {
                                            let ln = range.start + i + 1;
                                            let line_no = (range.start + i) as u64;

                                            let line_el: AnyElement = if let Some(all) =
                                                all_matches_opt.as_deref()
                                            {
                                                let on_line =
                                                    slice_for_line_range(all, line_no, line_no + 1);
                                                if on_line.is_empty() {
                                                    div().child(line).into_any_element()
                                                } else {
                                                    // Absolute match index into `all` for the
                                                    // first match on this line (used to tag
                                                    // the active one).
                                                    let base_ix =
                                                        all.partition_point(|m| m.line < line_no);
                                                    render_line_with_matches(
                                                        &line,
                                                        on_line,
                                                        base_ix,
                                                        current_match_ix,
                                                        match_bg.into(),
                                                        active_bg.into(),
                                                        active_fg.into(),
                                                    )
                                                }
                                            } else {
                                                div().child(line).into_any_element()
                                            };

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
                                                .child(line_el)
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
                        );

                    if find_open {
                        content_view.child(self.render_find_bar(cx)).into_any()
                    } else {
                        content_view.into_any()
                    }
                }
            }
        };

        // ── Status bar ────────────────────────────────────────────────────────
        let status_text: String = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::File(mf) => {
                    let top_line = tab.scroll_handle.0.borrow().base_handle.top_item() + 1;
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
                TabContent::Scratch => format!("{}  │  Ln 1  /  1", tab.title),
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
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .child(menu_bar)
            .child(tab_bar)
            .child(content)
            .child(status_bar)
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() {
    let app = Application::new().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);

        cx.bind_keys([
            KeyBinding::new("ctrl-f", Find, None),
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-f", Find, None),
            KeyBinding::new("escape", FindClose, Some("FindBar")),
            KeyBinding::new("shift-enter", FindPrev, Some("FindBar")),
        ]);

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
