//! The selected wiki: its family and language, its edits a minute, and its
//! last edits, newest first, each page a link. A user page, whose title
//! would name a person, shows as one without a link.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, link::Link, tag::Tag};
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};

use super::{
    pulse::{RecentEdit, WikiDetail},
    strip::family_color,
    ui::{PANEL_PADDING, figures, signed_bytes, thousands},
};

/// The detail of `detail`, or how to get one.
pub fn render_detail(detail: Option<&WikiDetail>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let Some(detail) = detail else {
        return div()
            .p(PANEL_PADDING)
            .text_color(theme.muted_foreground)
            .child("Select a wiki to see its last edits.")
            .into_any_element();
    };
    let info = &detail.info;
    let about = match (&*info.language, info.lang.is_empty()) {
        ("", _) => info.family.label().to_owned(),
        (language, true) => format!("{} · {language}", info.family.label()),
        (language, false) => format!("{} · {language} ({})", info.family.label(), info.lang),
    };
    v_flex()
        .min_h_0()
        .flex_1()
        .child(
            h_flex()
                .px(PANEL_PADDING)
                .py(px(8.))
                .gap(px(8.))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_none()
                        .size(px(9.))
                        .rounded_full()
                        .bg(family_color(info.family, theme)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme.muted_foreground)
                        .child(about),
                )
                .child(figures(cx).flex_none().text_color(theme.foreground).child(
                    SharedString::from(format!(
                        "{}/min · {} edits",
                        detail.per_minute,
                        thousands(detail.edits)
                    )),
                )),
        )
        .child(
            v_flex()
                .id("recent-edits")
                .min_h_0()
                .flex_1()
                .overflow_y_scroll()
                .children(
                    detail
                        .recent
                        .iter()
                        .enumerate()
                        .map(|(ix, edit)| render_edit(ix, edit, cx)),
                ),
        )
        .into_any_element()
}

fn render_edit(ix: usize, edit: &RecentEdit, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let delta_color = if edit.delta < 0 {
        theme.danger
    } else {
        theme.success
    };
    h_flex()
        .px(PANEL_PADDING)
        .h(px(28.))
        .gap(px(10.))
        .border_b_1()
        .border_color(theme.table_row_border)
        .child(
            figures(cx)
                .flex_none()
                .text_color(theme.muted_foreground)
                .child(SharedString::from(edit.time.clone())),
        )
        .child(
            figures(cx)
                .flex_none()
                .w(px(68.))
                .flex()
                .justify_end()
                .text_color(delta_color)
                .child(signed_bytes(edit.delta)),
        )
        .child(div().min_w_0().flex_1().overflow_hidden().map(|cell| {
            match &edit.page {
                Some(page) => cell.child(
                    Link::new(ElementId::NamedInteger("recent-edit".into(), ix as u64))
                        .href(SharedString::from(page.url.clone()))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(SharedString::from(page.title.clone())),
                ),
                None => cell
                    .text_color(theme.muted_foreground)
                    .child("a page not shown"),
            }
        }))
        .when(edit.new_page, |row| {
            row.child(Tag::success().outline().xsmall().child("new"))
        })
        .when(edit.bot, |row| {
            row.child(Tag::warning().outline().xsmall().child("bot"))
        })
}
