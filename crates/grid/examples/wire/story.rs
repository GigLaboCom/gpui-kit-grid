//! The story pane: the open story's category and time, its headline, its
//! neutral simulated body and the instruments it mentions as chips; a chip
//! makes its instrument current, and the quotes and the chart below follow.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Size};
use gpui_kit::{
    Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::LiveGrid;

use crate::{
    Wire, chart,
    newsroom::DISCLAIMER,
    ui::{self, CARD_PADDING},
};

/// The most instruments a story mentions.
const MOST_MENTIONED: usize = 4;

impl Wire {
    pub(crate) fn render_story(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let current = self.bus.read(cx).current();
        let story = self.news.read(cx).story().cloned();
        ui::card(cx).size_full().map(|card| match story {
            None => card.items_center().justify_center().gap(px(4.)).child(
                v_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().text_color(theme.foreground).child("No story open"))
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("Select a headline to read it here."),
                    ),
            ),
            Some(story) => card.child(
                v_flex()
                    .id("story")
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(20.))
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .text_size(px(12.))
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.primary)
                                    .child(story.category.label().to_uppercase()),
                            )
                            .child(div().text_color(theme.muted_foreground).child(
                                SharedString::from(format!(
                                    "{} UTC · {}",
                                    story.time,
                                    ui::SIMULATED
                                )),
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(20.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(SharedString::from(story.headline.to_string())),
                    )
                    .children(
                        story
                            .paragraphs()
                            .filter(|paragraph| *paragraph != DISCLAIMER)
                            .map(|paragraph| {
                                div()
                                    .line_height(px(22.))
                                    .text_color(theme.foreground.opacity(0.86))
                                    .child(SharedString::from(paragraph.to_owned()))
                            }),
                    )
                    .when(!story.tickers.is_empty(), |pane| {
                        pane.child(
                            v_flex()
                                .gap(px(8.))
                                .pt(px(4.))
                                .child(ui::section_label("Mentioned", cx))
                                .child(h_flex().flex_wrap().gap(px(8.)).children(
                                    story.tickers.iter().map(|&id| {
                                        let ticker = id.get().ticker;
                                        ui::chip(
                                            ("ticker", usize::from(id.0)),
                                            ticker,
                                            current == Some(id),
                                            cx,
                                        )
                                        .tooltip(id.get().name)
                                        .on_click(
                                            cx.listener(move |wire, _, _, cx| {
                                                wire.bus.update(cx, |bus, cx| {
                                                    bus.set_current(Some(id), cx)
                                                });
                                            }),
                                        )
                                    }),
                                )),
                        )
                    })
                    .child(
                        div()
                            .pt(px(4.))
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(theme.muted_foreground)
                            .child(DISCLAIMER),
                    ),
            ),
        })
    }

    /// The instruments the open story mentions, live, over the current
    /// instrument's chart.
    pub(crate) fn render_mentioned(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let mentioned = self
            .news
            .read(cx)
            .story()
            .map_or(0, |story| story.tickers.len());
        // The header and a row per instrument; the chart takes the rest.
        let rows = mentioned.clamp(1, MOST_MENTIONED) + 1;
        let height = crate::markets::ROW_HEIGHT * rows as f32 + px(2.);
        ui::card(cx)
            .size_full()
            .child(ui::card_header(
                "Mentioned instruments",
                Some(ui::SIMULATED.into()),
                cx,
            ))
            .map(|card| {
                if mentioned == 0 {
                    card.child(
                        div()
                            .flex_none()
                            .px(CARD_PADDING)
                            .text_color(theme.muted_foreground)
                            .child("The open story names no instrument; the chart keeps the current one."),
                    )
                } else {
                    card.child(
                        div().flex_none().h(height).px(ui::GRID_INSET).child(
                            LiveGrid::new(&self.mentioned)
                                .with_size(Size::Size(crate::markets::ROW_HEIGHT))
                                .bordered(false)
                                .accessibility_label("Mentioned instruments"),
                        ),
                    )
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(CARD_PADDING)
                    .pt(px(12.))
                    .pb(CARD_PADDING)
                    .child(self.chart.clone()),
            )
    }

    /// The current instrument's chart in a card of its own.
    pub(crate) fn render_chart_card(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        chart::card(&self.chart, cx)
    }
}
