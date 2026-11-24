use std::path::Path;

use eframe::egui::{Theme, Ui};

use crate::theme::{self, ColorRole, ThemePlugin};
use crate::{CAUTION, caption, primary, secondary, status};

pub(crate) struct ThemeEditor {
    draft: Option<ThemePlugin>,
    mode: Theme,
    message: Option<Result<&'static str, String>>,
}

impl Default for ThemeEditor {
    fn default() -> Self {
        Self {
            draft: None,
            mode: Theme::Dark,
            message: None,
        }
    }
}

impl ThemeEditor {
    pub(crate) fn show(&mut self, ui: &mut Ui, data_dir: Option<&Path>) {
        ui.collapsing("Custom theme", |ui| {
            if self.draft.is_none() {
                self.mode = ui.ctx().theme();
                self.draft = Some(match theme::load_plugin(data_dir) {
                    Ok(plugin) => plugin.unwrap_or_default(),
                    Err(error) => {
                        self.message = Some(Err(error));
                        ThemePlugin::default()
                    }
                });
            }
            self.contents(ui, data_dir);
        });
    }

    fn contents(&mut self, ui: &mut Ui, data_dir: Option<&Path>) {
        caption(ui, "Edit the colors or import a Render Bridge JSON theme.");
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Import theme", true).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Render Bridge theme", &["json"])
                    .pick_file()
            {
                self.message = Some(theme::import_plugin(data_dir, &path).map(|plugin| {
                    theme::apply_plugin(ui.ctx(), Some(plugin.clone()));
                    self.draft = Some(plugin);
                    "Theme imported."
                }));
            }
            if secondary(ui, "Reset to defaults", true).clicked() {
                self.message = Some(theme::reset_plugin(data_dir).map(|()| {
                    theme::apply_plugin(ui.ctx(), None);
                    self.draft = Some(ThemePlugin::default());
                    "Default colors restored."
                }));
            }
        });

        let Some(plugin) = self.draft.as_mut() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label("Theme name");
            ui.text_edit_singleline(&mut plugin.name);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Colors for");
            ui.selectable_value(&mut self.mode, Theme::Dark, "Dark");
            ui.selectable_value(&mut self.mode, Theme::Light, "Light");
        });
        eframe::egui::Grid::new("custom-theme-colors")
            .num_columns(3)
            .show(ui, |ui| {
                for role in ColorRole::ALL {
                    let [red, green, blue, _] =
                        plugin.color(self.mode, role).to_srgba_unmultiplied();
                    let mut rgb = [red, green, blue];
                    ui.label(role.label());
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        plugin.set_color(self.mode, role, rgb);
                    }
                    let [r, g, b] = rgb;
                    ui.monospace(format!("#{r:02X}{g:02X}{b:02X}"));
                    ui.end_row();
                }
            });
        ui.horizontal_wrapped(|ui| {
            if primary(ui, "Apply theme", true).clicked() {
                self.message = Some(theme::save_plugin(data_dir, plugin).map(|()| {
                    theme::apply_plugin(ui.ctx(), Some(plugin.clone()));
                    "Theme applied."
                }));
            }
            if secondary(ui, "Export theme", true).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Render Bridge theme", &["json"])
                    .set_file_name("render-bridge-theme.json")
                    .save_file()
            {
                self.message = Some(theme::write_plugin(&path, plugin).map(|()| "Theme exported."));
            }
        });
        match &self.message {
            Some(Ok(message)) => caption(ui, message),
            Some(Err(error)) => status(ui, "Theme", error, CAUTION),
            None => {}
        }
    }
}
