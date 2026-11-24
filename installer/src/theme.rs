use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Color32, Stroke, Theme, ThemePreference};
use serde::{Deserialize, Serialize};

use crate::setup;
use crate::{
    CAUTION, CREAM, ERROR, GREEN, INK, LIME, LINE, MARK_SHADE, MUTED, RAIL, RAIL_ACTIVE, RAIL_TEXT,
    SURFACE, VIOLET,
};

const PREFERENCE_FILE: &str = "appearance-theme";
const PLUGIN_FILE: &str = "appearance-palette.json";
const MAX_PLUGIN_BYTES: u64 = 16 * 1024;
const PLUGIN_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ColorRole {
    Background,
    Surface,
    Rail,
    Selection,
    RailText,
    Text,
    Muted,
    Border,
    Accent,
    Primary,
    Success,
    Warning,
    Error,
    Destructive,
    Logo,
}

impl ColorRole {
    pub(crate) const ALL: [Self; 15] = [
        Self::Background,
        Self::Surface,
        Self::Rail,
        Self::Selection,
        Self::RailText,
        Self::Text,
        Self::Muted,
        Self::Border,
        Self::Accent,
        Self::Primary,
        Self::Success,
        Self::Warning,
        Self::Error,
        Self::Destructive,
        Self::Logo,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Background => "Background",
            Self::Surface => "Panels and fields",
            Self::Rail => "Navigation",
            Self::Selection => "Selection",
            Self::RailText => "Navigation text",
            Self::Text => "Text",
            Self::Muted => "Secondary text",
            Self::Border => "Borders",
            Self::Accent => "Links and outlines",
            Self::Primary => "Primary actions",
            Self::Success => "Success",
            Self::Warning => "Warning",
            Self::Error => "Error text",
            Self::Destructive => "Destructive actions",
            Self::Logo => "Logo accent",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(try_from = "String", into = "String")]
struct HexColor([u8; 3]);

impl TryFrom<String> for HexColor {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let invalid = "Expected a color in #RRGGBB format.";
        let hex = value.strip_prefix('#').ok_or(invalid)?;
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid);
        }
        let number = u32::from_str_radix(hex, 16).map_err(|_| invalid)?;
        let [_, red, green, blue] = number.to_be_bytes();
        Ok(Self([red, green, blue]))
    }
}

impl From<HexColor> for String {
    fn from(color: HexColor) -> Self {
        let [r, g, b] = color.0;
        format!("#{r:02X}{g:02X}{b:02X}")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemePlugin {
    version: u32,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dark: BTreeMap<ColorRole, HexColor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    light: BTreeMap<ColorRole, HexColor>,
}

impl Default for ThemePlugin {
    fn default() -> Self {
        Self {
            version: PLUGIN_VERSION,
            name: "Custom".into(),
            dark: BTreeMap::new(),
            light: BTreeMap::new(),
        }
    }
}

impl ThemePlugin {
    fn validate(&self) -> Result<(), String> {
        if self.version != PLUGIN_VERSION {
            return Err("This theme uses an unsupported format version.".into());
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err("Enter a theme name of 1 to 64 characters.".into());
        }
        Ok(())
    }

    pub(crate) fn set_color(&mut self, theme: Theme, role: ColorRole, rgb: [u8; 3]) {
        match theme {
            Theme::Dark => &mut self.dark,
            Theme::Light => &mut self.light,
        }
        .insert(role, HexColor(rgb));
    }

    pub(crate) fn color(&self, theme: Theme, role: ColorRole) -> Color32 {
        let colors = match theme {
            Theme::Dark => &self.dark,
            Theme::Light => &self.light,
        };
        match colors.get(&role).copied() {
            Some(HexColor([r, g, b])) => Color32::from_rgb(r, g, b),
            None => default_color(theme, role),
        }
    }
}

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

fn read_plugin(path: &Path) -> Result<Option<ThemePlugin>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not read the theme file.".into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_PLUGIN_BYTES {
        return Err("Choose a regular JSON theme file of 16 KB or less.".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(MAX_PLUGIN_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| "Could not read the theme file.")?;
    if bytes.len() as u64 > MAX_PLUGIN_BYTES {
        return Err("Choose a JSON theme file of 16 KB or less.".into());
    }
    let plugin: ThemePlugin = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid theme JSON. Check the schema and #RRGGBB colors.")?;
    plugin.validate()?;
    Ok(Some(plugin))
}

pub(crate) fn load_plugin(data_dir: Option<&Path>) -> Result<Option<ThemePlugin>, String> {
    read_plugin(&preference_directory(data_dir)?.join(PLUGIN_FILE))
}

pub(crate) fn write_plugin(path: &Path, plugin: &ThemePlugin) -> Result<(), String> {
    plugin.validate()?;
    let mut bytes =
        serde_json::to_vec_pretty(plugin).map_err(|_| "Could not encode the theme file.")?;
    bytes.push(b'\n');
    setup::atomic_write(path, &bytes).map_err(|_| "Could not save the theme file.".into())
}

pub(crate) fn save_plugin(data_dir: Option<&Path>, plugin: &ThemePlugin) -> Result<(), String> {
    let directory = preference_directory(data_dir)?;
    setup::create_private_dir(&directory)
        .map_err(|_| "Could not open the Render Bridge data folder.")?;
    write_plugin(&directory.join(PLUGIN_FILE), plugin)
}

pub(crate) fn import_plugin(data_dir: Option<&Path>, path: &Path) -> Result<ThemePlugin, String> {
    let plugin = read_plugin(path)?.ok_or("The selected theme file is unavailable.")?;
    save_plugin(data_dir, &plugin)?;
    Ok(plugin)
}

pub(crate) fn reset_plugin(data_dir: Option<&Path>) -> Result<(), String> {
    match fs::remove_file(preference_directory(data_dir)?.join(PLUGIN_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Could not reset the theme. Check the data folder permissions.".into()),
    }
}

#[cfg(test)]
fn configure(ctx: &egui::Context, preference: ThemePreference) {
    configure_with(ctx, preference, None);
}

pub fn configure_for(ctx: &egui::Context, preference: ThemePreference, data_dir: Option<&Path>) {
    configure_with(ctx, preference, load_plugin(data_dir).ok().flatten());
}

fn configure_with(ctx: &egui::Context, preference: ThemePreference, plugin: Option<ThemePlugin>) {
    ctx.options_mut(|options| options.fallback_theme = Theme::Dark);
    ctx.set_theme(preference);
    apply_plugin(ctx, plugin);
}

pub(crate) fn apply_plugin(ctx: &egui::Context, plugin: Option<ThemePlugin>) {
    let key = egui::Id::new(PLUGIN_FILE);
    let plugin = plugin.map(Arc::new);
    ctx.data_mut(|data| match &plugin {
        Some(plugin) => data.insert_temp(key, Arc::clone(plugin)),
        None => data.remove::<Arc<ThemePlugin>>(key),
    });
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            let color = |role| resolve_color(theme, role, plugin.as_deref());
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(13.0, 8.0);
            style.visuals.override_text_color = Some(color(ColorRole::Text));
            style.visuals.hyperlink_color = color(ColorRole::Accent);
            style.visuals.panel_fill = color(ColorRole::Background);
            style.visuals.window_fill = color(ColorRole::Surface);
            style.visuals.extreme_bg_color = color(ColorRole::Surface);
            style.visuals.warn_fg_color = color(ColorRole::Warning);
            style.visuals.error_fg_color = color(ColorRole::Error);
            style.visuals.selection.bg_fill = color(ColorRole::Selection);
            style.visuals.selection.stroke =
                Stroke::new(1.0_f32, on_fill(color(ColorRole::Selection)));
            style.visuals.widgets.inactive.bg_stroke =
                Stroke::new(1.0_f32, color(ColorRole::Border));
            style.visuals.widgets.inactive.bg_fill = color(ColorRole::Surface);
            style.visuals.widgets.inactive.weak_bg_fill = color(ColorRole::Surface);
            style.visuals.widgets.hovered.bg_stroke =
                Stroke::new(1.0_f32, color(ColorRole::Accent));
            style.visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, color(ColorRole::Accent));
            for state in [
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                state.bg_fill = color(ColorRole::Selection);
                state.weak_bg_fill = color(ColorRole::Selection);
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
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

pub fn color(ctx: &egui::Context, role: Color32) -> Color32 {
    let semantic = match role {
        RAIL => ColorRole::Rail,
        RAIL_ACTIVE => ColorRole::Selection,
        RAIL_TEXT => ColorRole::RailText,
        CREAM => ColorRole::Background,
        SURFACE => ColorRole::Surface,
        INK => ColorRole::Text,
        LINE => ColorRole::Border,
        MUTED => ColorRole::Muted,
        VIOLET => ColorRole::Accent,
        LIME => ColorRole::Primary,
        MARK_SHADE => ColorRole::Logo,
        GREEN => ColorRole::Success,
        CAUTION => ColorRole::Warning,
        ERROR => ColorRole::Error,
        _ => return role,
    };
    current_color(ctx, semantic)
}

pub fn destructive_fill(ctx: &egui::Context) -> Color32 {
    current_color(ctx, ColorRole::Destructive)
}

pub fn on_fill(fill: Color32) -> Color32 {
    let [r, g, b, _] = fill.to_srgba_unmultiplied();
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    if luminance >= 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn current_color(ctx: &egui::Context, role: ColorRole) -> Color32 {
    let plugin = ctx.data(|data| data.get_temp::<Arc<ThemePlugin>>(egui::Id::new(PLUGIN_FILE)));
    resolve_color(ctx.theme(), role, plugin.as_deref())
}

fn resolve_color(theme: Theme, role: ColorRole, plugin: Option<&ThemePlugin>) -> Color32 {
    match plugin {
        Some(plugin) => plugin.color(theme, role),
        None => default_color(theme, role),
    }
}

fn default_color(theme: Theme, role: ColorRole) -> Color32 {
    use ColorRole::*;
    if theme == Theme::Light {
        return match role {
            Background => CREAM,
            Surface => Color32::from_rgba_unmultiplied(255, 253, 248, 248),
            Rail => RAIL,
            Selection => RAIL_ACTIVE,
            RailText => RAIL_TEXT,
            Text => INK,
            Muted => MUTED,
            Border => LINE,
            Accent => VIOLET,
            Primary => Color32::from_rgb(109, 64, 197),
            Success => GREEN,
            Warning => CAUTION,
            Error | Destructive => ERROR,
            Logo => MARK_SHADE,
        };
    }
    match role {
        Background => Color32::BLACK,
        Surface => Color32::from_rgba_unmultiplied(14, 45, 44, 248),
        Rail => Color32::from_rgb(16, 42, 42),
        Selection => Color32::from_rgb(59, 41, 85),
        RailText => Color32::from_rgb(192, 208, 205),
        Text => Color32::from_rgb(243, 247, 245),
        Muted => Color32::from_rgb(168, 194, 189),
        Border => Color32::from_rgb(58, 105, 101),
        Accent => Color32::from_rgb(197, 170, 255),
        Primary => Color32::from_rgb(159, 122, 251),
        Success => Color32::from_rgb(109, 230, 192),
        Warning => Color32::from_rgb(245, 199, 146),
        Error => Color32::from_rgb(248, 148, 156),
        Destructive => Color32::from_rgb(162, 50, 67),
        Logo => Color32::from_rgb(104, 211, 194),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_defaults_keep_teal_surfaces_and_purple_actions() {
        let ctx = egui::Context::default();
        configure(&ctx, ThemePreference::Dark);

        assert_eq!(color(&ctx, CREAM), Color32::BLACK);
        assert_eq!(color(&ctx, RAIL), Color32::from_rgb(16, 42, 42));
        assert_eq!(color(&ctx, LIME), Color32::from_rgb(159, 122, 251));
    }

    #[test]
    fn local_plugin_persists_both_modes_and_reset_keeps_other_settings() {
        let fixture = tempfile::tempdir().unwrap();
        let data_dir = fixture.path().join("install");
        let other = fixture.path().join("other");
        let source = fixture.path().join("plugin.json");
        let mut plugin = ThemePlugin::default();
        plugin.set_color(Theme::Dark, ColorRole::Primary, [190, 110, 21]);
        plugin.set_color(Theme::Light, ColorRole::Background, [220, 242, 230]);
        write_plugin(&source, &plugin).unwrap();
        let active = import_plugin(Some(&data_dir), &source).unwrap();
        save(Some(&data_dir), ThemePreference::System).unwrap();

        assert_eq!(active, plugin);
        assert_eq!(load_plugin(Some(&data_dir)).unwrap(), Some(plugin));
        assert_eq!(load_plugin(Some(&other)).unwrap(), None);

        let ctx = egui::Context::default();
        configure_for(&ctx, load(Some(&data_dir)), Some(&data_dir));
        for (mode, role, expected) in [
            (Theme::Light, CREAM, Color32::from_rgb(220, 242, 230)),
            (Theme::Dark, LIME, Color32::from_rgb(190, 110, 21)),
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    system_theme: Some(mode),
                    ..Default::default()
                },
                |ctx| assert_eq!(color(ctx, role), expected),
            );
        }

        reset_plugin(Some(&data_dir)).unwrap();
        assert_eq!(load_plugin(Some(&data_dir)).unwrap(), None);
        assert_eq!(load(Some(&data_dir)), ThemePreference::System);
    }

    #[test]
    fn invalid_theme_imports_preserve_the_active_plugin() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("plugin.json");
        let installed_plugin = ThemePlugin::default();
        save_plugin(Some(fixture.path()), &installed_plugin).unwrap();
        for bad in [
            r#"{"version":2,"name":"test"}"#,
            r#"{"version":1,"name":"test","run":"unknown"}"#,
            r##"{"version":1,"name":"test","dark":{"primary":"#ff003344"}}"##,
            r##"{"version":1,"name":"test","dark":{"unknown":"#ff0033"}}"##,
        ] {
            fs::write(&source, bad).unwrap();
            assert!(import_plugin(Some(fixture.path()), &source).is_err());
            assert_eq!(
                load_plugin(Some(fixture.path())).unwrap(),
                Some(installed_plugin.clone())
            );
        }
        fs::write(&source, vec![b' '; MAX_PLUGIN_BYTES as usize + 1]).unwrap();
        assert!(import_plugin(Some(fixture.path()), &source).is_err());
    }

    #[test]
    fn filled_action_text_stays_legible_on_dark_and_bright_palettes() {
        assert_eq!(on_fill(Color32::from_rgb(0, 24, 23)), Color32::WHITE);
        assert_eq!(on_fill(Color32::from_rgb(200, 210, 250)), Color32::BLACK);
    }

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
