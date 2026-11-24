use eframe::egui::{self, Color32, Stroke, Theme};

use crate::{CAUTION, CREAM, ERROR, GREEN, INK, LINE, MUTED, RAIL, RAIL_ACTIVE, SURFACE, VIOLET};

pub fn configure(ctx: &egui::Context) {
    ctx.options_mut(|options| options.fallback_theme = Theme::Light);
    ctx.set_theme(egui::ThemePreference::System);
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            let color = |role| palette(theme, role);
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(13.0, 8.0);
            style.visuals.override_text_color = Some(color(INK));
            style.visuals.hyperlink_color = color(VIOLET);
            style.visuals.panel_fill = color(CREAM);
            style.visuals.window_fill = color(SURFACE);
            style.visuals.extreme_bg_color = color(SURFACE);
            style.visuals.selection.bg_fill = color(RAIL_ACTIVE);
            style.visuals.selection.stroke = Stroke::new(1.0_f32, color(VIOLET));
            style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, color(LINE));
            style.visuals.widgets.inactive.bg_fill = color(SURFACE);
            style.visuals.widgets.inactive.weak_bg_fill = color(SURFACE);
            style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, color(VIOLET));
            style.visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, color(VIOLET));
            for state in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                state.corner_radius = egui::CornerRadius::same(12);
            }
        });
    }
}

pub fn color(ctx: &egui::Context, role: Color32) -> Color32 {
    palette(ctx.theme(), role)
}

fn palette(theme: Theme, role: Color32) -> Color32 {
    if theme == Theme::Light {
        return if role == SURFACE {
            Color32::from_rgba_unmultiplied(255, 253, 248, 248)
        } else {
            role
        };
    }
    match role {
        RAIL => Color32::from_rgb(40, 29, 59),
        RAIL_ACTIVE => Color32::from_rgb(70, 52, 101),
        CREAM => Color32::from_rgb(23, 20, 33),
        SURFACE => Color32::from_rgba_unmultiplied(42, 35, 60, 240),
        INK => Color32::from_rgb(249, 242, 255),
        LINE => Color32::from_rgb(79, 65, 104),
        MUTED => Color32::from_rgb(198, 185, 216),
        VIOLET => Color32::from_rgb(201, 171, 252),
        GREEN => Color32::from_rgb(158, 224, 184),
        CAUTION => Color32::from_rgb(245, 199, 146),
        ERROR => Color32::from_rgb(250, 155, 161),
        _ => role,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_system_theme_without_making_the_window_background_translucent() {
        let ctx = egui::Context::default();
        configure(&ctx);
        assert_eq!(ctx.theme(), Theme::Light);
        for theme in [Theme::Dark, Theme::Light] {
            let input = egui::RawInput {
                system_theme: Some(theme),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                assert_eq!(ctx.theme(), theme);
                assert_eq!(color(ctx, CREAM).a(), 255);
                assert!(color(ctx, SURFACE).a() < 255);
            });
        }
    }
}
