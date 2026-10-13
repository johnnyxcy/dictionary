use std::path::Path;

use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Icon,
        button::{Button, ButtonVariants},
    },
    prelude::FluentBuilder,
    *,
};

pub struct StartupError(pub String);

impl Render for StartupError {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        error_panel(&self.0, None, cx)
    }
}

pub fn error_panel(message: &str, directory: Option<&Path>, cx: &App) -> Div {
    let diagnostic = message.to_owned();
    div()
        .flex()
        .flex_col()
        .size_full()
        .p_8()
        .gap_4()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .child(Icon::new(IconName::TriangleAlert).text_color(cx.theme().danger))
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Dictionary could not start"),
        )
        .child(div().text_sm().child(
            "Your history has not been reset. Resolve the problem below, then reopen Dictionary.",
        ))
        .child(
            div()
                .id("startup-diagnostic")
                .role(Role::Label)
                .aria_label(diagnostic.clone())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_4()
                .rounded_md()
                .bg(cx.theme().muted)
                .child(diagnostic.clone()),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .when_some(directory.map(Path::to_owned), |this, path| {
                    this.child(
                        Button::new("open-data-directory")
                            .icon(IconName::FolderOpen)
                            .label("Open data folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                })
                .child(
                    Button::new("copy-startup-diagnostic")
                        .icon(IconName::Copy)
                        .label("Copy details")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(diagnostic.clone()))
                        }),
                )
                .child(
                    Button::new("quit-after-startup-error")
                        .primary()
                        .label("Quit")
                        .on_click(|_, _, cx| cx.quit()),
                ),
        )
}
