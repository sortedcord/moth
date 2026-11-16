//! Navigation sidebar. Full width on wide windows; on narrow ones it collapses to an icon rail
//! (like YouTube's mini guide). The width animates between the two and the content switches
//! halfway.

use gpui::{ClickEvent, Context, FontWeight, Window, div, prelude::*, px, rgb};

use super::AppView;
use super::icons::{Icon, icon, play_icon};
use super::layout::{RAIL_W, SIDEBAR_W};
use super::theme::{ACCENT, HOVER, MUTED, ON_ACCENT, PANEL, TEXT};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Home,
    Settings,
}

impl Entry {
    fn icon(self) -> Icon {
        match self {
            Self::Home => Icon::Home,
            Self::Settings => Icon::Settings,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Settings => "Settings",
        }
    }

    fn open(self, view: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
        match self {
            Self::Home => view.go_home(window, cx),
            Self::Settings => view.open_settings(window, cx),
        }
    }
}

pub fn render(width: f32, active: Entry, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let collapsed = width < (RAIL_W + SIDEBAR_W) / 2.0;
    let item = |entry: Entry| {
        let is_active = entry == active;
        let base = div()
            .id(entry.label())
            .rounded_lg()
            .cursor_pointer()
            .text_color(rgb(if is_active { TEXT } else { MUTED }))
            .hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)))
            .when(is_active, |item| item.bg(rgb(HOVER)).font_weight(FontWeight::SEMIBOLD))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| entry.open(this, window, cx)));
        let glyph = |size| icon(entry.icon(), size, rgb(if is_active { ACCENT } else { MUTED }).into());
        if collapsed {
            base.flex()
                .flex_col()
                .items_center()
                .gap_1()
                .py_2p5()
                .text_size(px(10.5))
                .child(glyph(20.0))
                .child(entry.label())
        } else {
            base.flex().items_center().gap_3().px_3().py_2().text_sm().child(glyph(18.0)).child(entry.label())
        }
    };
    let logo_mark = div()
        .w(px(30.0))
        .h(px(21.0))
        .flex_none()
        .rounded(px(6.0))
        .bg(rgb(ACCENT))
        .flex()
        .items_center()
        .justify_center()
        .child(play_icon(9.0, rgb(ON_ACCENT).into()));
    let home = item(Entry::Home);
    let settings = item(Entry::Settings);

    div()
        .w(px(width))
        .flex_none()
        .h_full()
        .overflow_hidden()
        .whitespace_nowrap()
        .rounded(px(20.0))
        .bg(rgb(PANEL))
        .when(collapsed, |rail| rail.px_1p5().py_4())
        .when(!collapsed, |panel| panel.p_4())
        .flex()
        .flex_col()
        .gap_0p5()
        .child(if collapsed {
            div().flex().justify_center().pb_4().pt_1().child(logo_mark)
        } else {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pb_5()
                .pt_1()
                .child(logo_mark)
                .child(div().font_weight(FontWeight::SEMIBOLD).child("moth"))
        })
        .child(home)
        .child(div().flex_1())
        .child(settings)
}
