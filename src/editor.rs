//! Editor state: the `TextEditor` view, its tab list, and the methods that
//! mutate that state outside of find/rendering (tab management, file opening,
//! scrolling).

use gpui::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::find::FindState;
use crate::viewer::MappedFile;

pub enum TabContent {
    Scratch,
    File(MappedFile),
    Error(String),
}

pub struct TabEntry {
    pub(crate) title: String,
    pub(crate) content: TabContent,
    /// Scroll handle owned by this tab; passed to `uniform_list` each frame.
    pub(crate) scroll_handle: UniformListScrollHandle,
    /// Set to true once after a file opens so render can auto-focus the viewport.
    pub(crate) needs_focus: bool,
}

// Main view 

pub struct TextEditor {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) tabs: Vec<TabEntry>,
    pub(crate) active_tab: usize,
    /// Find bar; `None` when hidden. Bound to whatever tab was active when opened.
    pub(crate) find: Option<FindState>,
    /// Background tasks must be held alive (dropping a Task cancels it).
    pub(crate) _tasks: Vec<Task<()>>,
    /// Current font size in pixels; adjusted by zoom in/out.
    pub(crate) font_size: f32,
}

impl TextEditor {
    pub fn new(cx: &mut Context<Self>) -> Self {
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
            font_size: 13.0,
        }
    }

    // Zoom helpers

    pub(crate) fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + 1.0).min(48.0);
        cx.notify();
    }

    pub(crate) fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.font_size = (self.font_size - 1.0).max(8.0);
        cx.notify();
    }

    pub(crate) fn new_scratch_tab(&mut self, cx: &mut Context<Self>) {
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



    pub(crate) fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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

    pub(crate) fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
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
}
