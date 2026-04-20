//! Find-bar controller methods on `TextEditor` plus the find-related rendering
//! helpers (`render_find_bar`, `render_line_with_matches`).

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::editor::{TabContent, TextEditor};
use crate::find::{FindState, Match, search_in_mmap};
use crate::{FindClose, FindPrev};

impl TextEditor {
    // ── Find feature ──────────────────────────────────────────────────────────

    /// Ctrl+F: toggle the find bar for the active tab.
    ///
    /// If already open and bound to this tab, just refocus the input and
    /// select its contents (matches VS Code's behavior on repeated Ctrl+F).
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Spawn a background search for the current query/case flag. The task
    /// waits 500 ms before running so rapid keystrokes coalesce into a single
    /// search; each new call drops the previous task, cancelling its timer.
    /// Stale completions that still make it through are filtered using
    /// `search_gen`.
    pub(crate) fn kick_off_search(&mut self, cx: &mut Context<Self>) {
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
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
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

    pub(crate) fn find_next(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn find_prev(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn render_find_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
pub(crate) fn render_line_with_matches(
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
