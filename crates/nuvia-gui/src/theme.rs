#![allow(dead_code)]

use eframe::egui::{
    self, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Response,
    RichText, Rounding, Stroke, TextStyle, Ui, Vec2,
};

pub const BG: Color32 = Color32::from_rgb(7, 10, 16);
pub const BG_2: Color32 = Color32::from_rgb(10, 15, 24);
pub const SIDEBAR: Color32 = Color32::from_rgb(9, 13, 20);
pub const CARD: Color32 = Color32::from_rgb(15, 21, 31);
pub const CARD_2: Color32 = Color32::from_rgb(20, 28, 41);
pub const CARD_3: Color32 = Color32::from_rgb(25, 35, 50);
pub const BORDER: Color32 = Color32::from_rgb(30, 42, 58);
pub const TEXT: Color32 = Color32::from_rgb(228, 236, 246);
pub const MUTED: Color32 = Color32::from_rgb(124, 143, 166);
pub const ACCENT: Color32 = Color32::from_rgb(0, 245, 160);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0, 196, 130);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(6, 48, 38);
pub const PURPLE: Color32 = Color32::from_rgb(139, 108, 255);
pub const AMBER: Color32 = Color32::from_rgb(255, 190, 92);
pub const RED: Color32 = Color32::from_rgb(255, 92, 112);

pub fn install(ctx: &Context) {
    setup_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.faint_bg_color = CARD_2;
    visuals.extreme_bg_color = Color32::from_rgb(5, 8, 13);
    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.inactive.weak_bg_fill = CARD_2;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.hovered.weak_bg_fill = CARD_3;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT_SOFT);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(30, 42, 60);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.selection.bg_fill = ACCENT_DARK;
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = AMBER;
    visuals.error_fg_color = RED;

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.rounding = Rounding::same(10.0);
    }

    style.visuals = visuals;
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 9.0);
    style.spacing.interact_size = Vec2::new(44.0, 34.0);
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(15.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(15.0, FontFamily::Name("RajdhaniSemi".into())),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(28.0, FontFamily::Name("RajdhaniSemi".into())),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    ctx.set_style(style);
}

fn setup_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "rajdhani".into(),
        FontData::from_static(include_bytes!("../assets/Rajdhani-Medium.ttf")).into(),
    );
    fonts.font_data.insert(
        "rajdhani-semi".into(),
        FontData::from_static(include_bytes!("../assets/Rajdhani-SemiBold.ttf")).into(),
    );
    fonts.font_data.insert(
        "rajdhani-bold".into(),
        FontData::from_static(include_bytes!("../assets/Rajdhani-Bold.ttf")).into(),
    );

    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .expect("proportional family")
        .insert(0, "rajdhani".into());

    fonts.families.insert(
        FontFamily::Name("RajdhaniSemi".into()),
        vec!["rajdhani-semi".into(), "rajdhani".into()],
    );
    fonts.families.insert(
        FontFamily::Name("RajdhaniBold".into()),
        vec!["rajdhani-bold".into(), "rajdhani".into()],
    );

    ctx.set_fonts(fonts);
}

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("RajdhaniBold".into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("RajdhaniSemi".into()))
}

pub fn paint_background(ui: &Ui) {
    let rect = ui.max_rect();
    let painter = ui.painter();

    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), BG);
    mesh.colored_vertex(rect.right_top(), BG_2);
    mesh.colored_vertex(rect.left_bottom(), Color32::from_rgb(6, 9, 15));
    mesh.colored_vertex(rect.right_bottom(), BG_2);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));

    let grid = Color32::from_rgba_unmultiplied(120, 160, 190, 5);
    let mut x = rect.left();
    while x < rect.right() {
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0_f32, grid),
        );
        x += 42.0;
    }
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0_f32, grid),
        );
        y += 42.0;
    }

    painter.circle_filled(
        egui::pos2(rect.right() - 60.0, rect.top() + 20.0),
        140.0,
        Color32::from_rgba_unmultiplied(0, 245, 160, 5),
    );
    painter.circle_filled(
        egui::pos2(rect.left() + 30.0, rect.bottom() - 10.0),
        170.0,
        Color32::from_rgba_unmultiplied(139, 108, 255, 5),
    );
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::none()
        .fill(CARD)
        .rounding(Rounding::same(16.0))
        .stroke(Stroke::new(1.0_f32, BORDER))
        .inner_margin(Margin::same(18.0))
        .show(ui, add)
        .inner
}

pub fn card_accent<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::none()
        .fill(CARD)
        .rounding(Rounding::same(16.0))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0, 245, 160, 90),
        ))
        .inner_margin(Margin::same(18.0))
        .show(ui, add)
        .inner
}

pub fn section(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(
        RichText::new(title)
            .font(semibold(12.0))
            .color(MUTED)
            .strong(),
    );
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).size(12.0).color(MUTED));
    }
}

pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .font(semibold(16.0))
            .color(if enabled {
                Color32::from_rgb(3, 18, 13)
            } else {
                MUTED
            })
            .strong(),
    )
    .fill(if enabled { ACCENT } else { CARD_2 })
    .stroke(Stroke::new(1.0_f32, if enabled { ACCENT } else { BORDER }))
    .rounding(Rounding::same(12.0))
    .min_size(Vec2::new(ui.available_width(), 48.0));
    ui.add_enabled(enabled, button)
}

pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .font(semibold(15.0))
            .color(TEXT)
            .strong(),
    )
    .fill(Color32::from_rgb(48, 16, 24))
    .stroke(Stroke::new(1.0_f32, RED))
    .rounding(Rounding::same(12.0))
    .min_size(Vec2::new(ui.available_width(), 46.0));
    ui.add(button)
}

pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), semibold(9.5), color);
    let size = Vec2::new(galley.size().x + 8.0, 14.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        Rounding::same(3.0),
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 20),
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response
}

pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (option, label) in options {
            let selected = *value == *option;
            let button = egui::Button::new(RichText::new(*label).font(semibold(12.0)).color(
                if selected {
                    Color32::from_rgb(3, 18, 13)
                } else {
                    TEXT
                },
            ))
            .fill(if selected { ACCENT } else { CARD_2 })
            .stroke(Stroke::new(1.0_f32, if selected { ACCENT } else { BORDER }))
            .rounding(Rounding::same(9.0))
            .min_size(Vec2::new(0.0, 34.0));
            if ui.add(button).clicked() {
                *value = *option;
            }
        }
    });
}

pub fn key_text(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::new(12.0, FontFamily::Monospace))
            .color(MUTED),
    );
}
