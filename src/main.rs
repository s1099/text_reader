//! Binary entry point. All the real logic lives in the sibling library crate
//! (`src/lib.rs` and its modules); this file just bootstraps the application,
//! registers global keybindings, and opens the main window.

use gpui::*;
use gpui_component::Root;
use gpui_component_assets::Assets;

use text_editor::{Find, FindClose, FindPrev, TextEditor};

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
