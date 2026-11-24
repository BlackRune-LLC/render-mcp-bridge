use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, Stroke, Theme, ThemePreference};

use crate::setup;
use crate::{
    CAUTION, CREAM, ERROR, GREEN, INK, LIME, LINE, MARK_SHADE, MUTED, RAIL, RAIL_ACTIVE, RAIL_TEXT,
    SURFACE, VIOLET,
};

const PREFERENCE_FILE: &str = "appearance-theme";

fn preference_directory(data_dir: Option<&Path>) -> Result<PathBuf, String> {
    match data_dir {
        Some(directory) if directory.is_absolute() => Ok(directory.to_path_buf()),
        Some(_) => Err("The Render Bridge data directory must be an absolute path.".into()),
        None => setup::data_dir(),
    }
}

pub fn load(data_dir: Option<&Path>) -> ThemePreference {
    let saved = preference_directory(data_dir)
        .ok()
        .and_then(|directory| fs::read_to_string(directory.join(PREFERENCE_FILE)).ok());
    match saved.as_deref().map(str::trim) {
        Some("light") => ThemePreference::Light,
        Some("system") => ThemePreference::System,
        _ => ThemePreference::Dark,
    }
}

pub fn save(data_dir: Option<&Path>, preference: ThemePreference) -> Result<(), String> {
    let directory = preference_directory(data_dir)?;
    let content = match preference {
        ThemePreference::Dark => b"dark\n".as_slice(),
        ThemePreference::Light => b"light\n",
        ThemePreference::System => b"system\n",
    };
    setup::create_private_dir(&directory)
        .and_then(|()| setup::atomic_write(&directory.join(PREFERENCE_FILE), content))
        .map_err(|_| {
            "Could not save the appearance preference. Check the data folder permissions.".into()
        })
}

pub fn configure(ctx: &egui::Context, preference: ThemePreference) {
    ctx.options_mut(|options| options.fallback_theme = Theme::Dark);
    ctx.set_theme(preference);
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
            if theme == Theme::Dark {
                for state in [
                    &mut style.visuals.widgets.hovered,
                    &mut style.visuals.widgets.active,
                ] {
                    state.bg_fill = color(RAIL_ACTIVE);
                    state.weak_bg_fill = color(RAIL_ACTIVE);
                }
            }
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
        RAIL => Color32::from_rgb(6, 43, 41),
        RAIL_ACTIVE => Color32::from_rgb(23, 70, 64),
        RAIL_TEXT => Color32::from_rgb(173, 207, 199),
        CREAM => Color32::BLACK,
        SURFACE => Color32::from_rgba_unmultiplied(13, 57, 53, 240),
        INK => Color32::from_rgb(236, 255, 248),
        LINE => Color32::from_rgb(64, 135, 121),
        MUTED => Color32::from_rgb(173, 207, 199),
        VIOLET => Color32::from_rgb(101, 208, 186),
        LIME => Color32::from_rgb(30, 108, 97),
        MARK_SHADE => Color32::from_rgb(77, 169, 156),
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
    fn saves_each_theme_without_touching_the_installation_settings() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("selected-installation");
        let other = fixture.path().join("other-installation");
        std::fs::create_dir(&data).unwrap();
        let settings = data.join("settings.json");
        std::fs::write(&settings, b"fixture setup").unwrap();

        assert_eq!(load(Some(&data)), egui::ThemePreference::Dark);
        for preference in [
            egui::ThemePreference::Light,
            egui::ThemePreference::System,
            egui::ThemePreference::Dark,
        ] {
            save(Some(&data), preference).unwrap();
            assert_eq!(load(Some(&data)), preference);
            assert_eq!(load(Some(&other)), egui::ThemePreference::Dark);
            assert_eq!(std::fs::read(&settings).unwrap(), b"fixture setup");
        }
        std::fs::write(data.join(PREFERENCE_FILE), b"unknown").unwrap();
        assert_eq!(load(Some(&data)), egui::ThemePreference::Dark);
    }

    #[test]
    fn fixed_preference_takes_precedence_over_system_appearance() {
        let ctx = egui::Context::default();
        for (preference, system, selected) in [
            (egui::ThemePreference::Dark, Theme::Light, Theme::Dark),
            (egui::ThemePreference::Light, Theme::Dark, Theme::Light),
        ] {
            configure(&ctx, preference);
            let input = egui::RawInput {
                system_theme: Some(system),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| assert_eq!(ctx.theme(), selected));
        }
    }

    #[test]
    fn follows_system_theme_without_making_the_window_background_translucent() {
        let ctx = egui::Context::default();
        configure(&ctx, ThemePreference::System);
        assert_eq!(ctx.theme(), Theme::Dark);
        for theme in [Theme::Dark, Theme::Light] {
            let input = egui::RawInput {
                system_theme: Some(theme),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                assert_eq!(ctx.theme(), theme);
                let background = if theme == Theme::Dark {
                    Color32::BLACK
                } else {
                    CREAM
                };
                assert_eq!(color(ctx, CREAM), background);
                assert!(color(ctx, SURFACE).a() < 255);
            });
        }
    }
}
