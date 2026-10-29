//! Colors, spacing and the small painted widgets the UI is built from.
//!
//! egui follows the system light/dark setting, and `pal(ui)` picks the matching palette.
//! Icons are drawn with shapes so they need no font or image.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Theme, Ui,
    Vec2, vec2,
};

pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub line: Color32,
    pub text: Color32,
    pub sub: Color32,
    pub accent: Color32,
    /// Text and icons in the accent hue on a surface.
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub err: Color32,
    pub err_soft: Color32,
    pub off: Color32,
    pub disabled_fill: Color32,
    pub disabled_text: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const LIGHT: Palette = Palette {
    bg: rgb(0xF4F6FB),
    surface: rgb(0xFFFFFF),
    line: rgb(0xE3E8F2),
    text: rgb(0x0B1B4D),
    sub: rgb(0x55607F),
    accent: rgb(0x2D3FD0),
    accent_text: rgb(0x2D3FD0),
    accent_soft: rgb(0xE9ECFD),
    ok: rgb(0x17803A),
    ok_soft: rgb(0xE3F4E8),
    warn: rgb(0x7A4F00),
    warn_soft: rgb(0xFFF1CC),
    err: rgb(0xB3261E),
    err_soft: rgb(0xFDE8E6),
    off: rgb(0x9AA3BD),
    disabled_fill: rgb(0xE8EBF3),
    disabled_text: rgb(0x6B7391),
};

pub const DARK: Palette = Palette {
    bg: rgb(0x0F1426),
    surface: rgb(0x1A2036),
    line: rgb(0x2A3350),
    text: rgb(0xE8ECFA),
    sub: rgb(0x9AA5C6),
    accent: rgb(0x4F63F0),
    accent_text: rgb(0x9AA6FF),
    accent_soft: rgb(0x262E5C),
    ok: rgb(0x4CC38A),
    ok_soft: rgb(0x17382B),
    warn: rgb(0xF2C14E),
    warn_soft: rgb(0x3D3216),
    err: rgb(0xFF8A80),
    err_soft: rgb(0x4A2220),
    off: rgb(0x6B7391),
    disabled_fill: rgb(0x242B45),
    disabled_text: rgb(0x7F89AB),
};

pub fn pal(ui: &Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

/// The semibold face, for headings, names and button labels.
pub fn bold() -> egui::FontFamily {
    egui::FontFamily::Name("bold".into())
}

/// Inter replaces egui's thin default text font. The defaults stay as fallbacks for symbols,
/// and Hack stays the monospace font.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf"))),
    );
    fonts.font_data.insert(
        "Inter-SemiBold".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"))),
    );
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "Inter".into());
    let mut semibold = vec!["Inter-SemiBold".to_string()];
    semibold.extend(proportional.iter().filter(|f| *f != "Inter-SemiBold").cloned());
    fonts.families.insert(bold(), semibold);
    ctx.set_fonts(fonts);
}

/// Registers the look for both themes. Call once at startup.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, p) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = vec2(8.0, 8.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            style.spacing.interact_size.y = control_height();
            style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(15.0));
            style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(15.0));
            style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(13.0));
            style.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(22.0));
            style.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(13.0));

            let v = &mut style.visuals;
            v.panel_fill = p.bg;
            v.window_fill = p.surface;
            v.extreme_bg_color = p.surface;
            v.faint_bg_color = p.surface;
            v.selection.bg_fill = p.accent_soft;
            v.selection.stroke = Stroke::new(1.0, p.accent_text);
            v.hyperlink_color = p.accent_text;
            v.window_stroke = Stroke::new(1.0, p.line);
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.fg_stroke.color = p.text;
                w.corner_radius = CornerRadius::same(10);
            }
            v.widgets.inactive.weak_bg_fill = p.surface;
            v.widgets.inactive.bg_fill = p.surface;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
            v.widgets.hovered.weak_bg_fill = p.accent_soft;
            v.widgets.hovered.bg_fill = p.accent_soft;
            v.widgets.active.weak_bg_fill = p.accent_soft;
            v.widgets.active.bg_fill = p.accent_soft;
            v.widgets.noninteractive.fg_stroke.color = p.text;
        });
    }
}

/// Touch-sized controls on the phone, a little tighter on the desktop.
pub fn control_height() -> f32 {
    if cfg!(target_os = "android") { 44.0 } else { 38.0 }
}

/// A rounded white (or dark) panel that spans the available width.
pub fn card<R>(ui: &mut Ui, border: Option<Color32>, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    let stroke = match border {
        Some(color) => Stroke::new(1.5, color),
        None => Stroke::new(1.0, p.line),
    };
    egui::Frame::new()
        .fill(p.surface)
        .stroke(stroke)
        .corner_radius(16)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A tinted full-width message box (errors, requests, results).
pub fn banner<R>(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(16)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A horizontal row with a fixed minimum height. egui reads the minimum before the
/// closure runs, so it has to be set on a scope around the row.
pub fn row<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = height;
        ui.horizontal(add).inner
    })
    .inner
}

/// A section heading with an optional small note on the right.
pub fn section(ui: &mut Ui, title: &str, note: Option<&str>) {
    let p = pal(ui);
    ui.add_space(4.0);
    row(ui, 20.0, |ui| {
        ui.label(egui::RichText::new(title).family(bold()).size(16.0).color(p.text));
        if let Some(note) = note {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(note).small().color(p.sub));
            });
        }
    });
}

pub fn sub(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(egui::RichText::new(text).small().color(p.sub));
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    /// No fill, accent text.
    Text,
    /// No fill, red text.
    Danger,
}

/// A rounded button. Disabled buttons are drawn flat and do not react.
pub fn button(ui: &mut Ui, kind: Kind, icon: Option<Icon>, label: &str, enabled: bool) -> Response {
    let p = pal(ui);
    let height = control_height();
    let font = FontId::new(15.0, bold());
    let (fg, fill, stroke) = match (kind, enabled) {
        (_, false) => (p.disabled_text, if kind == Kind::Primary || kind == Kind::Secondary { p.disabled_fill } else { Color32::TRANSPARENT }, Stroke::NONE),
        (Kind::Primary, true) => (Color32::WHITE, p.accent, Stroke::NONE),
        (Kind::Secondary, true) => (p.accent_text, p.surface, Stroke::new(1.5, p.line)),
        (Kind::Text, true) => (p.accent_text, Color32::TRANSPARENT, Stroke::NONE),
        (Kind::Danger, true) => (p.err, Color32::TRANSPARENT, Stroke::NONE),
    };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, fg);
    let icon_w = if icon.is_some() { 16.0 + 6.0 } else { 0.0 };
    let pad = if matches!(kind, Kind::Text | Kind::Danger) { 10.0 } else { 16.0 };
    let min_w = if kind == Kind::Primary { 84.0 } else { 0.0 };
    let width = (galley.size().x + icon_w + 2.0 * pad).max(min_w);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), if enabled { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let fill = if enabled && response.hovered() {
            match kind {
                Kind::Primary => p.accent.gamma_multiply(0.88),
                _ => p.accent_soft,
            }
        } else {
            fill
        };
        let painter = ui.painter();
        let radius = CornerRadius::same(12);
        painter.rect_filled(rect, radius, fill);
        if stroke != Stroke::NONE {
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
        }
        let content_w = galley.size().x + icon_w;
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(icon) = icon {
            icon.paint(painter, Rect::from_min_size(Pos2::new(x, rect.center().y - 8.0), Vec2::splat(16.0)), fg, 2.0);
            x += icon_w;
        }
        painter.galley(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    }
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A square button that only shows an icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let p = pal(ui);
    let size = control_height();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let color = if response.hovered() { p.accent_text } else { p.sub };
    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(10), p.accent_soft);
    }
    icon.paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(22.0)), color, 2.0);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(label)
}

/// A rounded tile with an icon, used for files and devices.
pub fn tile(ui: &mut Ui, icon: Icon, size: f32, tint: Tint) {
    let p = pal(ui);
    let (fill, fg) = match tint {
        Tint::Accent => (p.accent_soft, p.accent_text),
        Tint::Muted => (p.disabled_fill, p.disabled_text),
        Tint::Error => (p.err_soft, p.err),
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same((size * 0.27) as u8), fill);
    icon.paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(size * 0.54)), fg, 1.8);
}

#[derive(Clone, Copy)]
pub enum Tint {
    Accent,
    Muted,
    Error,
}

/// A small colored dot followed by text, for device status.
pub fn status_line(ui: &mut Ui, color: Color32, text_color: Color32, text: &str) {
    row(ui, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(egui::RichText::new(text).size(13.0).color(text_color));
    });
}

/// A rounded label with a tinted background and a dot in front.
pub fn pill(ui: &mut Ui, fill: Color32, dot: Color32, fg: Color32, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::new(13.0, bold()), fg);
    let size = vec2(galley.size().x + 12.0 + 8.0 + 6.0 + 12.0, 28.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(14), fill);
    painter.circle_filled(Pos2::new(rect.left() + 16.0, rect.center().y), 4.0, dot);
    painter.galley(Pos2::new(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, fg);
}

/// A thin progress bar.
pub fn progress(ui: &mut Ui, fraction: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), p.accent_soft);
    let mut fill = rect;
    fill.set_width((rect.width() * fraction.clamp(0.0, 1.0)).max(8.0));
    ui.painter().rect_filled(fill, CornerRadius::same(4), p.accent);
}

/// The app icon, drawn at `size` points. The PNG is the icon master cropped to its edges.
pub fn logo(ui: &mut Ui, size: f32) {
    let texture = logo_texture(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
}

fn logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::NULL.with("airlynx-logo");
    if let Some(texture) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/airlynx-logo.png"))
        .expect("the bundled logo is a valid PNG");
    let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    let texture = ctx.load_texture("airlynx-logo", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

/// Draws a dashed rounded outline, used for drop zones.
#[cfg(not(target_os = "android"))]
pub fn dashed_rect(ui: &Ui, rect: Rect, color: Color32) {
    let r = 14.0;
    let mut points = Vec::new();
    let corners = [
        (Pos2::new(rect.right() - r, rect.top() + r), -90.0f32),
        (Pos2::new(rect.right() - r, rect.bottom() - r), 0.0),
        (Pos2::new(rect.left() + r, rect.bottom() - r), 90.0),
        (Pos2::new(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (center, start) in corners {
        points.extend(arc(center, r, start, start + 90.0));
    }
    points.push(points[0]);
    ui.painter().extend(Shape::dashed_line(&points, Stroke::new(1.5, color), 6.0, 5.0));
}

/// Points along a circular arc, angles in degrees (0 = right, 90 = down).
fn arc(center: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = 10;
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            center + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Computer,
    Macsplorer,
    Phone,
    Unknown,
    File,
    Close,
    Plus,
    Refresh,
    Send,
    Inbox,
    Sliders,
    Download,
    Folder,
    Warning,
}

impl Icon {
    /// Draws the icon into `rect`. The shapes are laid out on a 24 by 24 grid.
    pub fn paint(self, painter: &egui::Painter, rect: Rect, color: Color32, width: f32) {
        let stroke = Stroke::new(width, color);
        let k = rect.width() / 24.0;
        let at = |x: f32, y: f32| rect.min + vec2(x, y) * k;
        let line = |pts: &[(f32, f32)]| painter.add(Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), stroke));
        let poly = |pts: &[(f32, f32)]| painter.add(Shape::closed_line(pts.iter().map(|&(x, y)| at(x, y)).collect(), stroke));
        let rounded = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
            painter.rect_stroke(Rect::from_min_max(at(x0, y0), at(x1, y1)), r * k, stroke, StrokeKind::Inside);
        };
        match self {
            Icon::Computer => {
                rounded(3.0, 4.0, 21.0, 16.0, 2.0);
                line(&[(12.0, 16.0), (12.0, 20.0)]);
                line(&[(8.0, 20.0), (16.0, 20.0)]);
            }
            Icon::Macsplorer => {
                // A monitor with a folder on its screen.
                rounded(3.0, 4.0, 21.0, 16.0, 2.0);
                line(&[(12.0, 16.0), (12.0, 20.0)]);
                line(&[(8.0, 20.0), (16.0, 20.0)]);
                line(&[(7.0, 8.0), (7.0, 7.0), (10.0, 7.0), (11.0, 8.0), (17.0, 8.0), (17.0, 13.0), (7.0, 13.0), (7.0, 8.0)]);
            }
            Icon::Phone => {
                rounded(7.0, 2.0, 17.0, 22.0, 2.5);
                line(&[(11.0, 18.0), (13.0, 18.0)]);
            }
            Icon::Unknown => {
                painter.circle_stroke(at(12.0, 12.0), 8.0 * k, stroke);
            }
            Icon::File => {
                line(&[(14.0, 3.0), (7.0, 3.0), (7.0, 21.0), (19.0, 21.0), (19.0, 8.0), (14.0, 3.0), (14.0, 8.0), (19.0, 8.0)]);
            }
            Icon::Close => {
                line(&[(6.0, 6.0), (18.0, 18.0)]);
                line(&[(18.0, 6.0), (6.0, 18.0)]);
            }
            Icon::Plus => {
                line(&[(12.0, 5.0), (12.0, 19.0)]);
                line(&[(5.0, 12.0), (19.0, 12.0)]);
            }
            Icon::Refresh => {
                // Almost a full circle with a solid arrowhead at its clockwise end.
                let center = at(12.0, 12.0);
                let r = 7.5 * k;
                let end = 315.0f32.to_radians();
                painter.add(Shape::line(arc(center, r, 25.0, 315.0), stroke));
                let tip_base = center + vec2(end.cos(), end.sin()) * r;
                let forward = vec2(-end.sin(), end.cos());
                let outward = vec2(end.cos(), end.sin());
                let head = vec![
                    tip_base + forward * 5.5 * k,
                    tip_base + outward * 4.0 * k,
                    tip_base - outward * 4.0 * k,
                ];
                painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
            }
            Icon::Send => {
                line(&[(22.0, 2.0), (11.0, 13.0)]);
                poly(&[(22.0, 2.0), (15.0, 22.0), (11.0, 13.0), (2.0, 9.0)]);
            }
            Icon::Inbox => {
                poly(&[(3.0, 13.0), (6.0, 5.0), (18.0, 5.0), (21.0, 13.0), (21.0, 19.0), (3.0, 19.0)]);
                line(&[(3.0, 13.0), (8.0, 13.0), (9.0, 16.0), (15.0, 16.0), (16.0, 13.0), (21.0, 13.0)]);
            }
            Icon::Sliders => {
                line(&[(4.0, 7.0), (14.0, 7.0)]);
                line(&[(18.0, 7.0), (20.0, 7.0)]);
                line(&[(4.0, 17.0), (6.0, 17.0)]);
                line(&[(10.0, 17.0), (20.0, 17.0)]);
                painter.circle_stroke(at(16.0, 7.0), 2.0 * k, stroke);
                painter.circle_stroke(at(8.0, 17.0), 2.0 * k, stroke);
            }
            Icon::Download => {
                line(&[(12.0, 3.0), (12.0, 15.0)]);
                line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
                line(&[(5.0, 20.0), (19.0, 20.0)]);
            }
            Icon::Folder => {
                poly(&[(3.0, 6.0), (9.0, 6.0), (11.0, 8.0), (21.0, 8.0), (21.0, 19.0), (3.0, 19.0)]);
            }
            Icon::Warning => {
                poly(&[(12.0, 3.0), (22.0, 21.0), (2.0, 21.0)]);
                line(&[(12.0, 10.0), (12.0, 15.0)]);
                painter.circle_filled(at(12.0, 18.0), 1.0 * k + width * 0.3, color);
            }
        }
    }
}

/// Text anchored in a rect, for the nav rows.
pub fn text_at(ui: &Ui, pos: Pos2, align: Align2, text: &str, font: FontId, color: Color32) {
    ui.painter().text(pos, align, text, font, color);
}
