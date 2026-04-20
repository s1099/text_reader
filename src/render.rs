//! `Render` implementation for `TextEditor`: menu bar, tab bar, content area
//! (scratch/error/file), and status bar. Kept as a single block to preserve
//! the exact closure captures and element ordering of the original code.

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::sync::{Arc, RwLock};

use crate::editor::{TabContent, TextEditor};
use crate::find::{Match, slice_for_line_range};
use crate::find_ui::render_line_with_matches;
use crate::util::{format_file_size, format_number};
use crate::viewer::get_lines;
use crate::Find;

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

        // Edit / View / Help menus
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

        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                Tab::new().label(tab.title.clone()).suffix(
                    Button::new(ElementId::Integer(i as u64 + 1000))
                        .ghost()
                        .small()
                        // .label("×")
                        .icon(
                            Icon::new(IconName::Close)
                                .text_color(cx.theme().primary)
                        )
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

        // Content area
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

        // Status bar
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

        // Root layout
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
