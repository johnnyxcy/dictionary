mod ui;

use dictionary::{discovery::AppPaths, service::Service};
use gpui_kit::*;

fn main() -> anyhow::Result<()> {
    let paths = AppPaths::discover()?;
    let (service, events) = Service::start(paths)?;
    application().with_assets(assets::AllAssets).run(move |cx| {
        gpui_kit::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-l", ui::FocusLookup, None),
            KeyBinding::new("cmd-b", ui::ToggleSidebar, None),
            KeyBinding::new("cmd-,", ui::OpenSettings, None),
            KeyBinding::new("cmd-q", ui::Quit, None),
        ]);
        cx.on_action(|_: &ui::Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
        open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Dictionary".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| ui::DictionaryView::new(service, events, window, cx)),
        )
        .expect("Cannot open Dictionary window");
        cx.activate(true);
    });
    Ok(())
}
