mod menus;
mod ui;

use dictionary::{discovery::AppPaths, service::Service};
use gpui_kit::*;

fn main() -> anyhow::Result<()> {
    let startup = AppPaths::discover().and_then(|paths| {
        let data = paths.data.clone();
        Service::start(paths).map(|(service, events)| (service, events, data))
    });
    let app = application().with_assets(assets::AllAssets);
    app.on_reopen(menus::show_window);
    app.run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::sync_system_appearance(None, cx);
        menus::init(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(900.), px(620.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Dictionary".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        match startup {
            Ok((service, events, data)) => {
                open_window(options, cx, |window, cx| {
                    cx.new(|cx| ui::DictionaryView::new(service, events, data, window, cx))
                })
                .expect("Cannot open Dictionary window");
            }
            Err(error) => {
                open_window(options, cx, |_, cx| {
                    cx.new(|_| ui::startup::StartupError(format!("{error:#}")))
                })
                .expect("Cannot open Dictionary startup diagnostics");
            }
        }
        cx.activate(true);
    });
    Ok(())
}
