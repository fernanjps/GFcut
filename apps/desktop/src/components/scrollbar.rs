use gpui::{AnyElement, div, prelude::*, px};

use crate::theme::SemanticColors;

/// Renders a visible, sleek vertical scrollbar rail indicator for panels and tracks.
pub(crate) fn render_scrollbar_rail(colors: &SemanticColors, thumb_height_px: f32) -> AnyElement {
    div()
        .id("gfcut-scrollbar-rail")
        .flex_none()
        .w(px(7.0))
        .h_full()
        .py(px(3.0))
        .px(px(1.0))
        .bg(colors.background.opacity(0.3))
        .border_l_1()
        .border_color(colors.border.opacity(0.35))
        .child(
            div()
                .w_full()
                .h_full()
                .rounded(px(2.0))
                .bg(colors.border.opacity(0.2))
                .child(
                    div()
                        .w_full()
                        .h(px(thumb_height_px.max(24.0)))
                        .rounded(px(2.0))
                        .bg(colors.muted_foreground.opacity(0.42)),
                ),
        )
        .into_any_element()
}
