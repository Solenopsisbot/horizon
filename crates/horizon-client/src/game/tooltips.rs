//! The item and text tooltip box: vanilla's `TooltipRenderUtil` sprite pair,
//! the positioner and the text layout, split out of the station screens.
use crate::game::Game;
use horizon_client::hud::{HudBatch, rgba};
use horizon_client::items::ItemStack;

pub(crate) fn tooltip_position(
    (x, y): (f32, f32),
    (width, height): (f32, f32),
    (w, h): (f32, f32),
) -> (f32, f32) {
    let mut x = (x + 12.0).round();
    let mut y = (y - 12.0).round();
    if x + w > width - 4.0 {
        x = (x - 28.0 - w).max(4.0);
    }
    if x + w > width - 4.0 {
        x = (width - w - 4.0).max(4.0);
    }
    if y + h + 3.0 > height {
        y = height - h - 3.0;
    }
    y = y.max(4.0);
    (x.round(), y.round())
}

impl Game<'_> {
    /// The tooltip's text's top-left corner and the box's size: beside the
    /// cursor, kept on screen. `extra` is [minimum width, height after the text].
    pub(crate) fn tooltip_geometry(
        atlas: &horizon_client::hud::HudAtlas,
        lines: &[(horizon_client::text::Text, u32)],
        (x, y): (f32, f32),
        (width, height): (f32, f32),
        extra: [f32; 2],
    ) -> (f32, f32, f32, f32) {
        let w = lines
            .iter()
            .map(|(t, _)| atlas.font.width_text(t))
            .fold(0.0, f32::max)
            .max(extra[0]);
        let text_h = if lines.len() == 1 {
            8.0
        } else {
            8.0 + 2.0 + (lines.len() - 1) as f32 * 10.0
        };
        let h = text_h + extra[1];
        let (w, h) = (w.ceil(), h.ceil());
        let (x, y) = tooltip_position((x, y), (width, height), (w, h));
        (x, y, w, h)
    }
    /// The box and its frame sprites.
    pub(super) fn tooltip_box(
        &self,
        batch: &mut HudBatch,
        stack: Option<&ItemStack>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        let defaults = (0xf0100010, 0x505000ff, 0x5028007f);
        let (mut background, mut border_top, mut border_bottom) = defaults;
        self.native_tooltip_color(stack, &mut background, &mut border_top, &mut border_bottom);
        // The sprite pair is vanilla's; a native hook that changes the old
        // fill colours keeps its own background and border.
        if (background, border_top, border_bottom) == defaults {
            let style = stack.and_then(|stack| stack.tooltip_style());
            horizon_client::ui::tooltip_background(
                batch,
                &self.gui.atlas,
                style.as_deref(),
                x,
                y,
                w,
                h,
            );
            return;
        }
        let convert = |color: u32| {
            let [a, r, g, b] = color.to_be_bytes();
            rgba(r, g, b, a)
        };
        let (background, border_top, border_bottom) = (
            convert(background),
            convert(border_top),
            convert(border_bottom),
        );
        let atlas = &self.gui.atlas;
        batch.fill(atlas, x - 3.0, y - 4.0, w + 6.0, 1.0, background);
        batch.fill(atlas, x - 3.0, y + h + 3.0, w + 6.0, 1.0, background);
        batch.fill(atlas, x - 4.0, y - 3.0, w + 8.0, h + 6.0, background);
        batch.fill(atlas, x - 3.0, y - 3.0, w + 6.0, 1.0, border_top);
        batch.fill(atlas, x - 3.0, y + h + 2.0, w + 6.0, 1.0, border_bottom);
        batch.fill_gradient(
            atlas,
            (x - 3.0, y - 2.0, 1.0, h + 4.0),
            border_top,
            border_bottom,
        );
        batch.fill_gradient(
            atlas,
            (x + w + 2.0, y - 2.0, 1.0, h + 4.0),
            border_top,
            border_bottom,
        );
    }
    pub(super) fn tooltip_text(
        batch: &mut HudBatch,
        atlas: &horizon_client::hud::HudAtlas,
        lines: &[(horizon_client::text::Text, u32)],
        x: f32,
        y: f32,
    ) {
        let mut ty = y;
        for (i, (text, color)) in lines.iter().enumerate() {
            batch.text_styled(atlas, text, x, ty, *color, true);
            ty += if i == 0 { 12.0 } else { 10.0 };
        }
    }
}
