use gpui_kit::{
    component::{
        WindowExt,
        input::{Copy, Cut, Paste, Redo, SelectAll, Undo},
    },
    *,
};

use crate::ui;

actions!(
    dictionary,
    [
        About,
        ShowWindow,
        Hide,
        HideOthers,
        ShowAll,
        CloseWindow,
        Minimize,
        Zoom,
        DockNewLookup,
        DockHistory,
        DockAnalysis,
        DockSettings,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-l", ui::FocusLookup, None),
        KeyBinding::new("cmd-n", ui::NewLookup, None),
        KeyBinding::new("cmd-f", ui::FocusHistory, None),
        KeyBinding::new("cmd-shift-a", ui::FocusAnalysis, None),
        KeyBinding::new("cmd-b", ui::ToggleSidebar, None),
        KeyBinding::new("cmd-,", ui::OpenSettings, None),
        KeyBinding::new("cmd-q", ui::Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &ui::Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &ShowWindow, cx| show_window(cx));
    cx.on_action(|_: &CloseWindow, cx| {
        with_window(cx, |window, _| window.remove_window());
    });
    cx.on_action(|_: &Minimize, cx| {
        with_window(cx, |window, _| window.minimize_window());
    });
    cx.on_action(|_: &Zoom, cx| {
        with_window(cx, |window, _| window.zoom_window());
    });
    cx.on_action(|_: &About, cx| {
        with_window(cx, |window, cx| {
            if !window.has_active_dialog(cx) {
                window.open_dialog(cx, |dialog, _, _| {
                    dialog
                        .title("About JDict")
                        .w(px(380.))
                        .child(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .child("English ↔ Simplified Chinese dictionary with local history.")
                });
            }
        });
    });

    // Dock commands need global handlers: the app may have no key window while
    // hidden or minimized. Keep them separate from view actions so forwarding
    // cannot recurse if the window is displaying startup diagnostics.
    cx.on_action(|_: &DockNewLookup, cx| open_from_dock(&ui::NewLookup, cx));
    cx.on_action(|_: &DockHistory, cx| open_from_dock(&ui::FocusHistory, cx));
    cx.on_action(|_: &DockAnalysis, cx| open_from_dock(&ui::FocusAnalysis, cx));
    cx.on_action(|_: &DockSettings, cx| open_from_dock(&ui::OpenSettings, cx));

    cx.set_menus([
        Menu::new("JDict").items([
            MenuItem::action("About JDict", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", ui::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide JDict", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit JDict", ui::Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Lookup", ui::NewLookup),
            MenuItem::action("Focus Lookup", ui::FocusLookup),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Lookup", ui::FocusLookup),
            MenuItem::action("History", ui::FocusHistory),
            MenuItem::action("Analysis", ui::FocusAnalysis),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ui::ToggleSidebar),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Show JDict", ShowWindow),
        ]),
    ]);
    // Register after the main menu: GPUI's native menu actions share a registry.
    // macOS supplies the Dock's standard Options, Hide/Show, and Quit entries.
    cx.set_dock_menu(vec![
        MenuItem::action("Show JDict", ShowWindow),
        MenuItem::separator(),
        MenuItem::action("New Lookup", DockNewLookup),
        MenuItem::action("History", DockHistory),
        MenuItem::action("Analysis", DockAnalysis),
        MenuItem::separator(),
        MenuItem::action("Settings…", DockSettings),
    ]);
}

pub fn show_window(cx: &mut App) {
    cx.activate(true);
    with_window(cx, |window, _| window.activate_window());
}

fn open_from_dock(action: &dyn Action, cx: &mut App) {
    let action = action.boxed_clone();
    cx.activate(true);
    with_window(cx, move |window, cx| {
        window.activate_window();
        if !window.has_active_dialog(cx) && window.is_action_available(action.as_ref(), cx) {
            window.dispatch_action(action, cx);
        }
    });
}

fn with_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    // JDict owns one window, which may be inactive when invoked from Dock.
    if let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        // Global actions can be dispatched inside an existing window update.
        // Defer the window access until that borrow has been released.
        cx.defer(move |cx| {
            handle.update(cx, |_, window, cx| f(window, cx)).ok();
        });
    }
}
