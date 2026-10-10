//! Visual theme for the desktop window: a calm light palette with one accent
//! colour, rounded controls and a clear type scale.
//!
//! The canvas backend draws widgets with egui, so the theme is set on egui's
//! style. It is applied to both the light and dark styles, so the window looks
//! the same whatever the operating system's preference.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// Page background, behind the cards.
pub const PAGE: Color32 = Color32::from_rgb(0xF3, 0xF5, 0xF8);
/// Card and text field background.
const SURFACE: Color32 = Color32::WHITE;
/// Resting control fill (buttons, inputs).
const CONTROL: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF2);
/// Control fill under the pointer.
const CONTROL_HOVER: Color32 = Color32::from_rgb(0xDA, 0xE1, 0xEC);
/// Control fill while pressed.
const CONTROL_ACTIVE: Color32 = Color32::from_rgb(0xC8, 0xD2, 0xE3);
/// Control and card borders.
const BORDER: Color32 = Color32::from_rgb(0xD0, 0xD7, 0xE2);
/// Main text.
const INK: Color32 = Color32::from_rgb(0x1F, 0x29, 0x37);
/// Accent: selection, links and hover edges.
const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x6E, 0xF5);

/// Paint the page background over the whole window. Call before drawing the
/// content: the canvas draws only its own widgets and cards, so the area
/// behind them would otherwise show egui's default window colour.
pub fn paint_page(ui: &egui::Ui) {
    ui.painter()
        .rect_filled(ui.max_rect(), CornerRadius::same(0), PAGE);
}

/// Apply the theme to every style of `ctx`.
pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.visuals = visuals();
        // The canvas sizes each button from its own text measurement, which can
        // be a little narrower than egui's. Never wrap: a label may run a few
        // pixels past its button, but it does not split onto two lines.
        style.wrap_mode = Some(egui::TextWrapMode::Extend);

        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.text_edit_width = 260.0;

        let scale = [
            (
                TextStyle::Heading,
                FontId::new(24.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(15.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(12.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
        ];
        for (style_name, font) in scale {
            style.text_styles.insert(style_name, font);
        }
    });
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    let round = CornerRadius::same(6);

    v.panel_fill = PAGE;
    v.window_fill = SURFACE;
    v.extreme_bg_color = SURFACE;
    v.window_corner_radius = CornerRadius::same(10);
    v.override_text_color = Some(INK);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    let widgets = &mut v.widgets;
    for (state, fill) in [
        (&mut widgets.noninteractive, SURFACE),
        (&mut widgets.inactive, CONTROL),
        (&mut widgets.hovered, CONTROL_HOVER),
        (&mut widgets.active, CONTROL_ACTIVE),
        (&mut widgets.open, CONTROL),
    ] {
        state.weak_bg_fill = fill;
        state.bg_fill = fill;
        state.corner_radius = round;
        state.bg_stroke = Stroke::new(1.0, BORDER);
        state.fg_stroke = Stroke::new(1.0, INK);
    }
    widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v
}
