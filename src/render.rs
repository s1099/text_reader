//! `Render` implementation for `TextEditor`: menu bar, tab bar, content area
//! (scratch/error/file), and status bar.

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
use crate::{Find, ZoomIn, ZoomOut};

impl Render for TextEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
        let active = self.active_tab;

        // Auto-focus the viewport when a new file tab is first rendered.
        if self.tabs.get(active).map(|t| t.needs_focus).unwrap_or(false) {
            if let Some(tab) = self.tabs.get_mut(active) {
                tab.needs_focus = false;
            }
            self.focus_handle.focus(window);
        }

        let weak = cx.weak_entity();
        let (weak_new, weak_open) = (weak.clone(), weak.clone());

        let file_menu = Button::new("file-menu-btn")
            .label("File")
            .ghost()
            .dropdown_menu(move |menu, _, _| {
                let (w_new, w_open) = (weak_new.clone(), weak_open.clone());
                menu.item(PopupMenuItem::new("New Tab").icon(IconName::File).on_click(
                    move |_, _, cx| {
                        w_new.update(cx, |v, cx| v.new_scratch_tab(cx)).ok();
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
                                w_open.update(cx, |v, cx| v.open_file(path, cx)).ok();
                            }
                        }),
                )
                .separator()
                .item(PopupMenuItem::new("Exit").on_click(|_, _, cx| cx.quit()))
            });

        let weak_find = weak.clone();
        let edit_menu = Button::new("edit-menu-btn")
            .label("Edit")
            .ghost()
            .dropdown_menu(move |menu, _, _| {
                let w_find = weak_find.clone();
                menu.item(PopupMenuItem::new("Copy").icon(IconName::Copy).disabled(true))
                    .separator()
                    .item(PopupMenuItem::new("Find").icon(IconName::Search).on_click(
                        move |_, window, cx| {
                            w_find.update(cx, |v, cx| v.open_find(window, cx)).ok();
                        },
                    ))
            });

        let (weak_zoom_in, weak_zoom_out) = (weak.clone(), weak.clone());
        let view_menu = Button::new("view-menu-btn")
            .label("View")
            .ghost()
            .dropdown_menu(move |menu, _, _| {
                let (w_in, w_out) = (weak_zoom_in.clone(), weak_zoom_out.clone());
                menu.item(
                    PopupMenuItem::new("Zoom In")
                        .icon(IconName::Plus)
                        .on_click(move |_, _, cx| {
                            w_in.update(cx, |v, cx| v.zoom_in(cx)).ok();
                        }),
                )
                .item(
                    PopupMenuItem::new("Zoom Out")
                        .icon(IconName::Minus)
                        .on_click(move |_, _, cx| {
                            w_out.update(cx, |v, cx| v.zoom_out(cx)).ok();
                        }),
                )
            });

        let help_menu = Button::new("help-menu-btn")
            .label("Help")
            .ghost()
            .dropdown_menu(|menu, _, _| {
                menu.item(PopupMenuItem::new("About").icon(IconName::Info).disabled(true))
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
                        .icon(Icon::new(IconName::Close).text_color(cx.theme().primary))
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

        // Dispatch content rendering to a focused helper per tab type.
        let content: AnyElement = match &self.tabs[active].content {
            TabContent::Scratch => self.render_scratch(cx).into_any_element(),
            TabContent::Error(msg) => {
                let msg = msg.clone();
                self.render_error(&msg, cx).into_any_element()
            }
            TabContent::File(_) => self.render_file(active, cx),
        };

        // Status bar text
        let status_text = {
            let tab = &self.tabs[active];
            match &tab.content {
                TabContent::File(mf) => {
                    let top_line = tab.scroll_handle.0.borrow().base_handle.top_item() + 1;
                    let path_str = mf.path.to_string_lossy();
                    let size_str = format_file_size(mf.file_size());
                    let total = mf.total_lines();
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

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _window, cx| this.zoom_in(cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _window, cx| this.zoom_out(cx)))
            .child(menu_bar)
            .child(tab_bar)
            .child(content)
            .child(status_bar)
    }
}


impl TextEditor {
    /// Empty scratch tab placeholder.
    fn render_scratch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(self.font_size);
        let gutter_width = px(2.0 * self.font_size * 0.65 + 10.0);
        let gutter_color = cx.theme().foreground.opacity(0.4);

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
                    .text_size(font_size)
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
    }

    /// Error state shown when a file fails to open.
    fn render_error(&self, msg: &str, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .p_6()
            .font_family("monospace")
            .text_sm()
            .child(format!("Error opening file: {msg}"))
    }

    fn render_file(&mut self, active: usize, cx: &mut Context<Self>) -> AnyElement {
        // Pull out what we need from the file tab.
        let (total_lines, mmap_arc, index_arc) = {
            let TabContent::File(mf) = &self.tabs[active].content else {
                unreachable!()
            };
            (
                mf.total_lines() as usize,
                Arc::clone(&mf.mmap),
                Arc::clone(&mf.index),
            )
        };
        let scroll_handle = self.tabs[active].scroll_handle.clone();

        // Gutter sizing based on line-count digit width.
        let font_size = px(self.font_size);
        let char_width = self.font_size * 0.65;
        let num_digits = total_lines.max(1).to_string().len().max(2);
        let gutter_width = px(num_digits as f32 * char_width + 10.0);
        let gutter_color = cx.theme().foreground.opacity(0.4);

        // Snapshot find state before any mutable borrow.
        let (find_open, matches_arc, active_match_line, current_match_ix): (
            bool,
            Option<Arc<RwLock<Vec<Match>>>>,
            Option<u64>,
            Option<usize>,
        ) = match self.find.as_ref() {
            Some(f) if f.tab_index == active => {
                let m = Arc::clone(&f.matches);
                let active_line = m
                    .read()
                    .ok()
                    .and_then(|r| r.get(f.current).map(|mm| mm.line));
                (true, Some(m), active_line, Some(f.current))
            }
            _ => (false, None, None, None),
        };

        // Scroll active match into view exactly once per navigation.
        if let Some(f) = self.find.as_mut() {
            if f.tab_index == active && f.pending_scroll {
                f.pending_scroll = false;
                if let Some(line) = active_match_line {
                    self.tabs[active].scroll_handle.scroll_to_item_strict(
                        line.saturating_sub(3) as usize,
                        ScrollStrategy::Top,
                    );
                }
            }
        }

        // Find-match highlight colors.
        let match_bg = rgba(0x5a_4a_00_80); // muted amber
        let active_bg = rgba(0xff_9e_00_cc); // strong amber
        let active_fg = rgba(0x00_00_00_ff);

        let list = uniform_list(
            "file-content",
            total_lines.max(1),
            move |range, _window, _cx| {
                let lines = get_lines(&mmap_arc, &index_arc, range.start as u64, range.len());
                let all_matches_opt = matches_arc.as_ref().and_then(|a| a.read().ok());

                lines
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let ln = range.start + i + 1;
                        let line_no = (range.start + i) as u64;

                        let line_el: AnyElement = match all_matches_opt.as_deref() {
                            Some(all) => {
                                let on_line = slice_for_line_range(all, line_no, line_no + 1);
                                if on_line.is_empty() {
                                    div().child(line).into_any_element()
                                } else {
                                    let base_ix = all.partition_point(|m| m.line < line_no);
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
                            }
                            None => div().child(line).into_any_element(),
                        };

                        div()
                            .flex()
                            .flex_row()
                            .font_family("monospace")
                            .text_size(font_size)
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
        .track_scroll(scroll_handle.clone())
        .size_full();

        let content_view = div()
            .id("content-area")
            .flex_1()
            .overflow_hidden()
            .relative()
            .track_focus(&self.focus_handle)
            .child(list)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(Scrollbar::new(&scroll_handle)),
            );

        if find_open {
            content_view.child(self.render_find_bar(cx)).into_any()
        } else {
            content_view.into_any()
        }
    }
}
