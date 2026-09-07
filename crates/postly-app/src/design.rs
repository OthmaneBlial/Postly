//! Shared native design system. Both themes use explicit, opaque text colors.
use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextStyle};

pub const LIME: Color32 = Color32::from_rgb(202, 235, 131);
pub const INK: Color32 = Color32::from_rgb(19, 28, 25);

pub fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/postly-icon.png"))
        .expect("the bundled Postly icon must be a valid PNG")
}

pub fn syntax_colors(visuals: &egui::Visuals) -> [Color32; 4] {
    if visuals.dark_mode {
        [
            Color32::from_rgb(214, 166, 95),
            Color32::from_rgb(117, 194, 226),
            Color32::from_rgb(194, 139, 236),
            Color32::from_rgb(103, 190, 143),
        ]
    } else {
        [
            Color32::from_rgb(133, 67, 0),
            Color32::from_rgb(8, 100, 125),
            Color32::from_rgb(112, 64, 153),
            Color32::from_rgb(40, 96, 57),
        ]
    }
}

pub fn accent(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        LIME
    } else {
        Color32::from_rgb(48, 100, 36)
    }
}

pub fn sidebar(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(16, 22, 28)
    } else {
        Color32::from_rgb(235, 240, 243)
    }
}

pub fn install(ctx: &egui::Context) {
    let icon = app_icon();
    let texture = ctx.load_texture(
        "postly-brand",
        egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        ),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("postly-brand"), texture));
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Plex".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        "Plex Medium".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"))
            .into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "Plex".into());
    fonts.families.insert(
        egui::FontFamily::Name("Medium".into()),
        vec!["Plex Medium".into(), "Plex".into()],
    );
    ctx.set_fonts(fonts);
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.text_styles.insert(
                TextStyle::Heading,
                FontId::new(19.0, egui::FontFamily::Name("Medium".into())),
            );
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(14.0));
            style.text_styles.insert(
                TextStyle::Button,
                FontId::new(13.0, egui::FontFamily::Name("Medium".into())),
            );
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(11.0));
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(13.0));
            style.spacing.item_spacing = egui::vec2(8.0, 5.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size = egui::vec2(32.0, 28.0);
            style.spacing.window_margin = egui::Margin::same(18);
            style.spacing.menu_margin = egui::Margin::same(8);
            style.animation_time = 0.12;
            let dark = theme == egui::Theme::Dark;
            let (surface, field, hover, border, text, muted, accent, selection) = if dark {
                (
                    Color32::from_rgb(23, 31, 38),
                    Color32::from_rgb(30, 40, 49),
                    Color32::from_rgb(43, 57, 68),
                    Color32::from_rgb(49, 64, 75),
                    Color32::from_rgb(233, 239, 243),
                    Color32::from_rgb(166, 181, 192),
                    LIME,
                    Color32::from_rgb(44, 62, 43),
                )
            } else {
                (
                    Color32::WHITE,
                    Color32::from_rgb(243, 246, 248),
                    Color32::from_rgb(227, 235, 240),
                    Color32::from_rgb(198, 210, 219),
                    Color32::from_rgb(25, 42, 53),
                    Color32::from_rgb(77, 96, 109),
                    Color32::from_rgb(48, 100, 36),
                    Color32::from_rgb(221, 237, 207),
                )
            };
            let v = &mut style.visuals;
            v.panel_fill = surface;
            v.window_fill = surface;
            v.extreme_bg_color = field;
            v.text_edit_bg_color = Some(field);
            v.faint_bg_color = field;
            v.code_bg_color = field;
            v.weak_text_color = Some(muted);
            v.override_text_color = None;
            v.hyperlink_color = accent;
            v.selection.bg_fill = selection;
            v.selection.stroke = Stroke::new(1.0, accent);
            v.window_stroke = Stroke::new(1.0, border);
            v.window_corner_radius = egui::CornerRadius::same(12);
            v.menu_corner_radius = egui::CornerRadius::same(8);
            v.error_fg_color = if dark {
                Color32::from_rgb(255, 142, 130)
            } else {
                Color32::from_rgb(173, 44, 38)
            };
            v.warn_fg_color = if dark {
                Color32::from_rgb(242, 197, 114)
            } else {
                Color32::from_rgb(129, 80, 12)
            };
            for widget in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                widget.bg_fill = field;
                widget.weak_bg_fill = field;
                widget.bg_stroke = Stroke::new(1.0, border);
                widget.fg_stroke = Stroke::new(1.4, text);
                widget.corner_radius = egui::CornerRadius::same(6);
                widget.expansion = 0.0;
            }
            v.widgets.hovered.bg_fill = hover;
            v.widgets.hovered.weak_bg_fill = hover;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
            v.widgets.active.bg_fill = selection;
            v.widgets.active.weak_bg_fill = selection;
            v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
            v.widgets.open = v.widgets.active;
        });
    }
    ctx.set_theme(egui::ThemePreference::Dark);
}

pub fn frame(ui: &egui::Ui, margin: i8) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(margin)
}

pub fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if let Some(texture) = ui
            .ctx()
            .data(|data| data.get_temp::<egui::TextureHandle>(egui::Id::new("postly-brand")))
        {
            ui.image((texture.id(), egui::vec2(34.0, 34.0)));
        }
        ui.label(RichText::new("postly").size(24.0).strong());
    });
}

pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(INK))
        .fill(LIME)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(90.0, 38.0))
}

pub fn tab(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let color = if selected {
        accent(ui)
    } else {
        ui.visuals().weak_text_color()
    };
    let response = ui.add(
        egui::Button::new(RichText::new(label).color(color))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE),
    );
    if selected {
        ui.painter().line_segment(
            [response.rect.left_bottom(), response.rect.right_bottom()],
            Stroke::new(2.0, color),
        );
    }
    response
}

pub fn navigation_row(
    ui: &mut egui::Ui,
    selected: bool,
    method: &str,
    name: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 35.0), egui::Sense::click());
    if response.clicked() {
        response.request_focus();
    }
    if selected || response.hovered() || response.has_focus() {
        ui.painter().rect_filled(
            rect,
            6.0,
            if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.hovered.bg_fill
            },
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            6.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    let color = if method == "GET" {
        accent(ui)
    } else {
        ui.visuals().hyperlink_color
    };
    ui.painter().with_clip_rect(rect.shrink(4.0)).text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        method,
        FontId::monospace(11.0),
        color,
    );
    let label_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(65.0, 0.0),
        rect.max - egui::vec2(7.0, 0.0),
    );
    ui.painter().with_clip_rect(label_rect).text(
        label_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            format!("{method} {name}"),
        )
    });
    response.on_hover_text(format!("{method} {name}"))
}

pub fn empty_response(ui: &mut egui::Ui) {
    ui.add_space((ui.available_height() * 0.18).max(10.0));
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Ready when you are.").size(25.0).strong());
        ui.add_space(8.0);
        ui.label(
            RichText::new("Send a request to see the status, headers and response body.")
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new("CMD / CTRL + ENTER")
                .monospace()
                .small()
                .color(accent(ui)),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_brand_icon_has_expected_dimensions_and_transparency() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert_eq!(icon.rgba[3], 0, "rounded corners must be transparent");
        assert!(
            icon.rgba.chunks_exact(4).any(|pixel| {
                pixel[0] > 180 && pixel[1] > 200 && pixel[2] < 150 && pixel[3] == 255
            }),
            "the Postly lime mark must be present"
        );
    }
    fn luminance(c: Color32) -> f32 {
        let linear = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    }
    #[test]
    fn both_themes_keep_body_and_secondary_text_readable() {
        let ctx = egui::Context::default();
        install(&ctx);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = ctx.style_of(theme);
            for text in [style.visuals.text_color(), style.visuals.weak_text_color()]
                .into_iter()
                .chain(syntax_colors(&style.visuals))
            {
                for background in [style.visuals.panel_fill, style.visuals.extreme_bg_color] {
                    let a = luminance(text);
                    let b = luminance(background);
                    assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5);
                }
            }
        }
    }

    #[test]
    fn navigation_rows_support_pointer_and_keyboard_activation() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut rect = egui::Rect::NOTHING;
        let mut clicked = false;
        let mut draw = |events| {
            clicked = false;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(250.0, 120.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = navigation_row(ui, false, "GET", "List orders");
                    rect = response.rect;
                    clicked |= response.clicked();
                },
            )
            .drop_without_applying_deltas();
            (rect, clicked)
        };
        let (rect, _) = draw(vec![]);
        let position = rect.center();
        draw(vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert!(
            draw(vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE
            }])
            .1
        );
        assert!(
            draw(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE
            }])
            .1
        );
    }
}
