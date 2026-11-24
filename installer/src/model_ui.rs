use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, pos2,
};

use crate::model_download::{self, InstallOutcome, Target, Verification};
use crate::model_library::{self, Library};
use crate::models::{DOWNLOAD_PIPELINES, ModelFile, Pipeline};
use crate::{
    CAUTION, CREAM, GREEN, INK, LIME, LINE, MODEL_GUIDE, MUTED, RAIL_ACTIVE, SURFACE, VIOLET,
    activity, asset_ui, caption, card, field_label, primary, secondary, status, theme,
};

#[derive(PartialEq, Eq)]
struct Connection {
    url: String,
    default_root: PathBuf,
    data_dir: Option<PathBuf>,
}

struct Transfer {
    result: Receiver<Result<Work, String>>,
    cancel: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    name: &'static str,
    verifying: bool,
    resume: Vec<Target>,
    destination: Option<PathBuf>,
}

enum Work {
    Installed(InstallOutcome),
    Verified(Verification),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ModelTab {
    #[default]
    Installed,
    Packs,
    Downloads,
}

#[derive(Default)]
pub struct ModelManager {
    connection: Option<Connection>,
    selected: Option<PathBuf>,
    destination_changed: bool,
    library: Option<Library>,
    scan: Option<Receiver<Result<Library, String>>>,
    transfer: Option<Transfer>,
    notice: Option<(String, bool)>,
    installed_only: bool,
    workspace_tab: ModelTab,
    pack: usize,
    workspace_model: Option<ModelSelection>,
    paused: Option<(&'static str, Vec<Target>)>,
    search: String,
    category: Option<String>,
    verification: Option<(&'static str, Verification)>,
    confirm_repair: bool,
    pipeline_requested: bool,
}

impl ModelManager {
    pub fn downloading(&self) -> bool {
        self.transfer.is_some()
    }

    pub fn cancel(&self) {
        if let Some(transfer) = &self.transfer {
            transfer.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn destination_change(&self) -> Option<Option<&Path>> {
        self.destination_changed.then_some(self.selected.as_deref())
    }

    pub fn take_pipeline_request(&mut self) -> bool {
        std::mem::take(&mut self.pipeline_requested)
    }

    #[cfg(windows)]
    pub(crate) fn reload_saved_destination(&mut self) {
        let Some(data_dir) = self
            .connection
            .as_ref()
            .and_then(|connection| connection.data_dir.as_deref())
        else {
            return;
        };
        let destination = model_library::load_destination(data_dir);
        if self.selected != destination {
            self.selected = destination;
            self.verification = None;
            self.confirm_repair = false;
            self.paused = None;
        }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let Some(connection) = &self.connection else {
            return;
        };
        let url = connection.url.clone();
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("inspect-model-library".into())
            .spawn(move || {
                let _ = sender.send(model_library::inspect(&url));
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.scan = Some(result);
        } else {
            self.notice = Some(("Could not start the model check. Try again.".into(), false));
        }
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(scan) = &self.scan {
            match scan.try_recv() {
                Ok(Ok(library)) => {
                    self.scan = None;
                    self.library = Some(library);
                }
                Ok(Err(error)) => {
                    self.scan = None;
                    self.library = None;
                    self.notice = Some((error, false));
                }
                Err(TryRecvError::Disconnected) => {
                    self.scan = None;
                    self.notice =
                        Some(("The model check did not finish. Try again.".into(), false));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let Some(transfer) = &self.transfer else {
            return;
        };
        let outcome = match transfer.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err("The model operation stopped unexpectedly.".into())
            }
            _ => return,
        };
        let name = transfer.name;
        let verifying = transfer.verifying;
        if outcome
            .as_ref()
            .is_err_and(|error| error == model_download::ABORTED)
            && !verifying
            && transfer.destination == self.selected
        {
            self.paused = Some((name, transfer.resume.clone()));
        }
        self.transfer = None;
        match outcome {
            Ok(Work::Verified(report)) => self.verification = Some((name, report)),
            Ok(Work::Installed(result)) => {
                self.verification = None;
                let mut message = if result.files == 0 {
                    "The model files are already on disk. Checking the render backend.".into()
                } else {
                    format!(
                        "Saved {} model file(s). Checking the render backend.",
                        result.files
                    )
                };
                for backup in result.backups {
                    message.push_str(&format!("\nOriginal kept at {}", backup.display()));
                }
                self.notice = Some((message, true));
            }
            Err(error) if error == model_download::ABORTED => {
                let message = if verifying {
                    "Verification stopped."
                } else {
                    "Download paused. Partial files were kept."
                };
                self.notice = Some((message.into(), true));
            }
            Err(error) => self.notice = Some((error, false)),
        }
        if !verifying {
            self.refresh(ctx);
        }
    }

    fn start_work(
        &mut self,
        name: &'static str,
        targets: Vec<Target>,
        verifying: bool,
        ctx: &egui::Context,
    ) {
        let repairing = targets.iter().any(Target::is_repair);
        let resume = if verifying {
            Vec::new()
        } else {
            targets.clone()
        };
        let total = Arc::new(AtomicU64::new(targets.iter().map(Target::bytes).sum()));
        let cancel = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let worker_cancel = Arc::clone(&cancel);
        let worker_bytes = Arc::clone(&bytes);
        let worker_total = Arc::clone(&total);
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        if thread::Builder::new()
            .name("download-model-files".into())
            .spawn(move || {
                let progress = |done, size| {
                    worker_total.store(size, Ordering::Relaxed);
                    worker_bytes.store(done, Ordering::Relaxed);
                };
                let outcome = if verifying {
                    model_download::verify(&targets, &worker_cancel, progress).map(Work::Verified)
                } else {
                    model_download::install(&targets, &worker_cancel, progress).map(Work::Installed)
                };
                let _ = sender.send(outcome);
                ctx.request_repaint();
            })
            .is_ok()
        {
            self.transfer = Some(Transfer {
                result,
                cancel,
                bytes,
                total,
                name,
                verifying,
                resume,
                destination: self.selected.clone(),
            });
            if !verifying {
                self.paused = None;
            }
            self.notice = None;
            if !repairing {
                self.verification = None;
            }
            self.confirm_repair = false;
        } else {
            self.notice = Some((
                "Could not start the model download. Try again.".into(),
                false,
            ));
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        url: Option<&str>,
        default_root: PathBuf,
        data_dir: Option<&Path>,
    ) {
        self.show_models(ui, url, default_root, data_dir, false);
    }

    pub fn show_workspace(
        &mut self,
        ui: &mut Ui,
        url: Option<&str>,
        default_root: PathBuf,
        data_dir: Option<&Path>,
    ) {
        self.show_models(ui, url, default_root, data_dir, true);
    }

    fn show_models(
        &mut self,
        ui: &mut Ui,
        url: Option<&str>,
        default_root: PathBuf,
        data_dir: Option<&Path>,
        workspace: bool,
    ) {
        let connection = url.map(|url| Connection {
            url: url.into(),
            default_root,
            data_dir: data_dir.map(Path::to_owned),
        });
        let changed_connection = self.connection != connection;
        let previous_transfer = changed_connection && self.downloading();
        if changed_connection && !previous_transfer {
            self.selected = data_dir.and_then(model_library::load_destination);
            self.destination_changed = false;
            self.connection = connection;
            self.library = None;
            self.scan = None;
            self.notice = None;
            self.verification = None;
            self.confirm_repair = false;
            self.category = None;
            self.workspace_model = None;
            self.paused = None;
            self.pipeline_requested = false;
            self.refresh(ui.ctx());
        }
        if workspace {
            self.workspace(ui, previous_transfer);
            return;
        }
        if self.connection.is_none() {
            caption(
                ui,
                "Choose a running ComfyUI build in Render Bridge Setup to manage its models.",
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.installed_only, false, "Download");
            ui.selectable_value(&mut self.installed_only, true, "Installed");
            if secondary(ui, "Refresh", self.scan.is_none() && !self.downloading()).clicked() {
                self.notice = None;
                self.verification = None;
                self.confirm_repair = false;
                self.refresh(ui.ctx());
            }
        });
        self.transfer_card(ui);
        if previous_transfer {
            caption(
                ui,
                "This operation uses the previous ComfyUI build. Finish or stop it to view the current library.",
            );
            return;
        }
        if let Some((message, ok)) = &self.notice {
            if *ok {
                caption(ui, message);
            } else {
                status(ui, "Model manager", message, CAUTION);
            }
        }
        if self.scan.is_some() {
            activity(ui, "Checking the active model library…");
        }
        if self.installed_only {
            self.show_setup_inventory(ui);
        } else {
            self.show_setup_catalog(ui);
        }
    }

    fn show_setup_inventory(&mut self, ui: &mut Ui) {
        let Some(library) = &self.library else {
            return;
        };
        let label = field_label(ui, "Find installed models");
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .desired_width(ui.available_width().min(280.0)),
        )
        .labelled_by(label);
        let search = self.search.trim().to_lowercase();
        let mut count = 0;
        for (category, files) in &library.files {
            let matching: Vec<_> = files
                .iter()
                .filter(|name| matches_installed_query(category, name, None, &search))
                .collect();
            if matching.is_empty() {
                continue;
            }
            count += matching.len();
            egui::CollapsingHeader::new(format!(
                "{} ({})",
                category_name(category),
                matching.len()
            ))
            .default_open(!search.is_empty() || library.files.len() <= 5)
            .show(ui, |ui| {
                for name in matching {
                    ui.add(egui::Label::new(RichText::new(name).monospace()).wrap());
                }
            });
        }
        if count == 0 {
            caption(
                ui,
                if search.is_empty() {
                    "This ComfyUI build reported no model files."
                } else {
                    "No installed models match this search."
                },
            );
        }
    }

    fn show_setup_catalog(&mut self, ui: &mut Ui) {
        let picker = |this: &mut Self, ui: &mut Ui| {
            for (index, pack) in DOWNLOAD_PIPELINES.iter().enumerate() {
                let (label, _) = pack_status(this.library.as_ref(), pack);
                let label = theme::strong(format!("{}\n{}", pack.name, label), 13.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 46.0],
                        egui::Button::selectable(this.pack == index, label).wrap(),
                    )
                    .on_hover_text(pack.description)
                    .clicked()
                {
                    this.pack = index;
                    this.confirm_repair = false;
                }
            }
            this.show_model_storage(ui, true);
        };
        if ui.available_width() >= 570.0 {
            ui.columns(2, |columns| {
                picker(self, &mut columns[0]);
                self.show_pack_details(&mut columns[1], self.pack, false);
            });
        } else {
            picker(self, ui);
            self.show_pack_details(ui, self.pack, false);
        }
    }

    fn workspace(&mut self, ui: &mut Ui, previous_transfer: bool) {
        if self.connection.is_none() {
            ui.heading("Model library");
            caption(
                ui,
                "Choose a running ComfyUI build in Render Bridge Setup to manage its models.",
            );
            return;
        }
        let width = ui.available_width();
        let rail = width >= 840.0;
        let split = width >= 690.0;
        let gap = 12.0;
        let rail_width = 184.0;
        let inspector_width = (width * 0.34).clamp(254.0, 355.0);
        if rail {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                pane(ui, rail_width, |ui| self.workspace_rail(ui));
                let library_width = width - rail_width - inspector_width - gap * 2.0;
                pane(ui, library_width, |ui| {
                    self.workspace_content(ui, previous_transfer)
                });
                if !previous_transfer {
                    pane(ui, inspector_width, |ui| self.workspace_inspector(ui));
                }
            });
        } else {
            self.workspace_tabs(ui);
            if split && !previous_transfer {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    pane(ui, width - inspector_width - gap, |ui| {
                        self.workspace_content(ui, false)
                    });
                    pane(ui, inspector_width, |ui| self.workspace_inspector(ui));
                });
            } else {
                self.workspace_content(ui, previous_transfer);
                if !previous_transfer {
                    self.workspace_inspector(ui);
                }
            }
        }
    }

    fn workspace_rail(&mut self, ui: &mut Ui) {
        let frame = Frame::new()
            .fill(theme::color(ui.ctx(), SURFACE))
            .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
            .corner_radius(12)
            .inner_margin(Margin::same(11));
        frame.show(ui, |ui| {
            ui.set_min_height(486.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            for (tab, text, glyph) in model_views() {
                if nav_row(ui, text, glyph, self.workspace_tab == tab).clicked() {
                    self.workspace_tab = tab;
                    self.confirm_repair = false;
                }
            }
            if self.workspace_tab == ModelTab::Downloads {
                return;
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("model-facets")
                .max_height(325.0)
                .show(ui, |ui| {
                    if nav_row(
                        ui,
                        "All categories",
                        ModelGlyph::Layers,
                        self.category.is_none(),
                    )
                    .clicked()
                    {
                        self.category = None;
                    }
                    for category in categories(self.library.as_ref(), self.workspace_tab) {
                        let selected = self.category.as_deref() == Some(category.as_str());
                        if nav_row(
                            ui,
                            &category_name(&category),
                            category_glyph(&category),
                            selected,
                        )
                        .clicked()
                        {
                            self.category = Some(category);
                        }
                    }
                });
        });
    }

    fn workspace_tabs(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            for (tab, text, _) in model_views() {
                if ui
                    .selectable_label(self.workspace_tab == tab, text)
                    .clicked()
                {
                    self.workspace_tab = tab;
                    self.confirm_repair = false;
                }
            }
            if self.workspace_tab != ModelTab::Downloads {
                let current = self
                    .category
                    .as_deref()
                    .map(category_name)
                    .unwrap_or_else(|| "All categories".into());
                egui::ComboBox::from_id_salt("workspace-model-facet")
                    .selected_text(current)
                    .width(125.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.category, None, "All categories");
                        for category in categories(self.library.as_ref(), self.workspace_tab) {
                            let text = category_name(&category);
                            ui.selectable_value(&mut self.category, Some(category), text);
                        }
                    });
            }
        });
    }

    fn workspace_content(&mut self, ui: &mut Ui, previous_transfer: bool) {
        let tab = self.workspace_tab;
        ui.horizontal_wrapped(|ui| {
            theme::heading(
                ui,
                match tab {
                    ModelTab::Installed => "Model library",
                    ModelTab::Packs => "Model packs",
                    ModelTab::Downloads => "Downloads",
                },
                24.0,
            );
            if ui
                .add_enabled(
                    self.scan.is_none() && !self.downloading(),
                    egui::Button::new("Refresh"),
                )
                .clicked()
            {
                self.notice = None;
                self.verification = None;
                self.confirm_repair = false;
                self.refresh(ui.ctx());
            }
        });
        if previous_transfer {
            self.transfer_card(ui);
            status(
                ui,
                "Previous connection",
                "Finish or pause the current operation before opening this ComfyUI library.",
                CAUTION,
            );
            return;
        }
        if let Some((message, ok)) = &self.notice {
            if *ok {
                caption(ui, message);
            } else {
                status(ui, "Model manager", message, CAUTION);
            }
        }
        if self.scan.is_some() {
            activity(ui, "Checking the active model library…");
        }
        if tab == ModelTab::Downloads {
            self.workspace_downloads(ui);
            return;
        }

        let facets = categories(self.library.as_ref(), tab);
        if self
            .category
            .as_ref()
            .is_some_and(|value| !facets.contains(value))
        {
            self.category = None;
        }
        self.model_search(ui);
        ui.add_space(4.0);
        if let Some(transfer) = &self.transfer {
            let done = transfer.bytes.load(Ordering::Relaxed);
            let total = transfer.total.load(Ordering::Relaxed);
            let progress = transfer_ratio(done, total);
            let name = transfer.name;
            let action = if transfer.verifying {
                "Verifying"
            } else {
                "Downloading"
            };
            let clicked = card_row(
                ui,
                &format!("{action} {name}"),
                &format!(
                    "{} of {}  ·  {:.0}%",
                    file_size(done),
                    file_size(total),
                    progress * 100.0
                ),
                None,
                false,
                RowPreview::Glyph(ModelGlyph::Download),
            )
            .clicked();
            if clicked {
                self.workspace_tab = ModelTab::Downloads;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }

        let query = self.search.trim().to_lowercase();
        let category_filter = self.category.as_deref();
        let visible = self.workspace_items(tab, category_filter, &query);
        if self
            .workspace_model
            .as_ref()
            .is_none_or(|selected| !visible.contains(selected))
        {
            self.workspace_model = visible.first().cloned();
            self.confirm_repair = false;
        }
        if visible.is_empty() {
            if self.scan.is_none() {
                let searching = !query.is_empty() || category_filter.is_some();
                card(ui, |ui| {
                    if searching {
                        field_label(ui, "No matching models");
                        if secondary(ui, "Clear filters", true).clicked() {
                            self.search.clear();
                            self.category = None;
                        }
                    } else if self.library.is_some() && tab == ModelTab::Installed {
                        field_label(ui, "No models installed");
                        if secondary(ui, "Browse packs", true).clicked() {
                            self.workspace_tab = ModelTab::Packs;
                        }
                    }
                });
            }
            return;
        }

        let pack_count = visible
            .iter()
            .take_while(|entry| matches!(entry, ModelSelection::Pack(_)))
            .count();
        for item in &visible[..pack_count] {
            self.workspace_item_row(ui, item);
        }
        let raw_files = &visible[pack_count..];
        if !raw_files.is_empty() {
            if pack_count > 0 {
                ui.add_space(4.0);
                ui.label(RichText::new("Model files").strong());
            }
            let available = ui.available_height();
            let viewport = ui.clip_rect().height();
            let preferred = if available.is_finite() {
                available
            } else {
                viewport * 0.55
            };
            let list_height = preferred.clamp(128.0, 520.0);
            egui::ScrollArea::vertical()
                .id_salt("workspace-model-files")
                .max_height(list_height)
                .min_scrolled_height(128.0)
                .show_rows(ui, FILE_ROW_HEIGHT, raw_files.len(), |ui, rows| {
                    for index in rows {
                        self.workspace_item_row(ui, &raw_files[index]);
                    }
                });
        }
    }

    fn model_search(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(theme::color(ui.ctx(), SURFACE))
            .stroke(Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
            .corner_radius(9)
            .inner_margin(Margin::symmetric(9, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                    draw_glyph(
                        ui.painter(),
                        rect.center(),
                        ModelGlyph::Search,
                        theme::color(ui.ctx(), MUTED),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .desired_width(ui.available_width())
                            .frame(false)
                            .hint_text("Search models or filenames"),
                    );
                });
            });
    }

    fn workspace_items(
        &self,
        tab: ModelTab,
        category: Option<&str>,
        query: &str,
    ) -> Vec<ModelSelection> {
        let mut visible = Vec::new();
        let catalog = [0, 2, 1];
        for index in catalog {
            let pack = &DOWNLOAD_PIPELINES[index];
            let eligible = |file: &ModelFile| {
                tab == ModelTab::Packs
                    || self
                        .library
                        .as_ref()
                        .is_some_and(|library| library.has(file))
            };
            let matching_category = pack.files.iter().any(|file| {
                eligible(file) && category.is_none_or(|kind| model_category(file) == kind)
            });
            let matching_text = pack.name.to_lowercase().contains(query)
                || pack.files.iter().any(|file| {
                    eligible(file)
                        && matches_installed_query(
                            model_category(file),
                            file.filename,
                            category,
                            query,
                        )
                });
            if matching_category && matching_text {
                visible.push(ModelSelection::Pack(index));
            }
        }
        if tab == ModelTab::Installed
            && let Some(library) = &self.library
        {
            for (kind, names) in &library.files {
                for name in names
                    .iter()
                    .filter(|name| matches_installed_query(kind, name, category, query))
                {
                    visible.push(ModelSelection::File {
                        category: kind.clone(),
                        name: name.clone(),
                    });
                }
            }
        }
        visible
    }

    fn workspace_item_row(&mut self, ui: &mut Ui, item: &ModelSelection) {
        let selected = self.workspace_model.as_ref() == Some(item);
        let response = match item {
            ModelSelection::Pack(index) => {
                let pack = &DOWNLOAD_PIPELINES[*index];
                let (label, color) = pack_status(self.library.as_ref(), pack);
                card_row(
                    ui,
                    pack.name,
                    pack_type(pack.name),
                    Some((&label, color)),
                    selected,
                    RowPreview::Pack(pack.name),
                )
            }
            ModelSelection::File { category, name } => card_row(
                ui,
                filename(name),
                &category_name(category),
                None,
                selected,
                RowPreview::Glyph(category_glyph(category)),
            )
            .on_hover_text(name),
        };
        if response.clicked() {
            self.workspace_model = Some(item.clone());
            self.confirm_repair = false;
        }
    }

    fn workspace_inspector(&mut self, ui: &mut Ui) {
        let selected = if self.workspace_tab == ModelTab::Downloads {
            self.transfer
                .as_ref()
                .map(|task| task.name)
                .or_else(|| self.paused.as_ref().map(|(name, _)| *name))
                .and_then(|name| DOWNLOAD_PIPELINES.iter().position(|pack| pack.name == name))
                .map(ModelSelection::Pack)
        } else {
            self.workspace_model.clone()
        };
        let pack_index = match &selected {
            Some(ModelSelection::Pack(index)) => Some(*index),
            _ => None,
        };
        match selected {
            Some(ModelSelection::Pack(index)) => self.show_pack_details(ui, index, true),
            Some(ModelSelection::File { category, name }) => card(ui, |ui| {
                ui.heading(filename(&name));
                caption(ui, &category_name(&category));
                ui.separator();
                field_label(ui, "Registered filename");
                crate::code_label(ui, &name);
                if secondary(ui, "Copy filename", true).clicked() {
                    ui.ctx().copy_text(name);
                }
            }),
            None => {}
        }

        ui.add_space(5.0);
        theme::heading(ui, "Model storage", 18.0);
        card(ui, |ui| {
            if let Some(folder) = &self.selected {
                field_label(ui, "Preferred model folder");
                crate::code_label(ui, &folder.to_string_lossy());
            } else {
                field_label(ui, "Automatic");
                caption(
                    ui,
                    "Uses the folders registered by the active ComfyUI build.",
                );
            }
            self.show_model_storage(ui, false);
        });
        if let Some(index) = pack_index {
            self.show_pack_files(ui, index);
        }
    }

    fn show_pack_files(&self, ui: &mut Ui, index: usize) {
        let Some(pack) = DOWNLOAD_PIPELINES.get(index) else {
            return;
        };
        egui::CollapsingHeader::new("View files")
            .id_salt(("model-pack-files", pack.name))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for file in pack.files {
                    let category = model_category(file);
                    let (state, color) = match &self.library {
                        Some(lib) if lib.has(file) => ("Installed", GREEN),
                        Some(lib) if lib.reports_category(file) => ("Missing", CAUTION),
                        _ => ("Unconfirmed", MUTED),
                    };
                    let label = format!("{} · {}", category_name(category), state);
                    egui::CollapsingHeader::new(
                        theme::strong(label, 12.0).color(theme::color(ui.ctx(), color)),
                    )
                    .id_salt((pack.name, file.filename))
                    .show(ui, |ui| crate::code_label(ui, file.filename));
                }
                caption(ui, pack.description);
                caption(ui, pack.license);
            });
    }

    fn workspace_downloads(&mut self, ui: &mut Ui) {
        if self.downloading() {
            self.transfer_card(ui);
        } else if let Some((name, targets)) = &self.paused {
            let resume = card_action(
                ui,
                name,
                "Paused. The existing partial files will be reused.",
                "Resume download",
                self.scan.is_none(),
            );
            if resume {
                self.start_work(name, targets.clone(), false, ui.ctx());
            }
        } else if card_action(
            ui,
            "No downloads running",
            "Choose a pack to install it here.",
            "Browse packs",
            true,
        ) {
            self.workspace_tab = ModelTab::Packs;
        }
    }

    fn transfer_card(&self, ui: &mut Ui) {
        let Some(transfer) = &self.transfer else {
            return;
        };
        let done = transfer.bytes.load(Ordering::Relaxed);
        let total = transfer.total.load(Ordering::Relaxed);
        let ratio = transfer_ratio(done, total);
        let cancelling = transfer.cancel.load(Ordering::Relaxed);
        card(ui, |ui| {
            ui.heading(transfer.name);
            caption(
                ui,
                if transfer.verifying {
                    "Verifying files"
                } else {
                    "Downloading"
                },
            );
            ui.add(
                egui::ProgressBar::new(ratio)
                    .fill(theme::color(ui.ctx(), LIME))
                    .show_percentage(),
            );
            ui.horizontal_wrapped(|ui| {
                caption(ui, &format!("{} of {}", file_size(done), file_size(total)));
                let label = if cancelling {
                    "Stopping…"
                } else if transfer.verifying {
                    "Stop verification"
                } else {
                    "Pause download"
                };
                if secondary(ui, label, !cancelling).clicked() {
                    self.cancel();
                }
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    fn show_model_storage(&mut self, ui: &mut Ui, expanded: bool) {
        let downloading = self.downloading();
        if let Some(connection) = &self.connection {
            egui::CollapsingHeader::new("Model storage and Hugging Face").default_open(expanded).show(ui, |ui| {
                ui.add_enabled_ui(!downloading, |ui| {
                    let changed = asset_ui::folder_option(
                        ui,
                        "Model storage folder",
                        &mut self.selected,
                        "Automatic. Use folders registered by the active ComfyUI build.",
                    );
                    self.destination_changed |= changed;
                    if changed {
                        self.verification = None;
                        self.confirm_repair = false;
                        self.paused = None;
                    }
                    if changed && let Some(data_dir) = &connection.data_dir
                        && let Err(error) = model_library::save_destination(data_dir, self.selected.as_deref())
                    {
                        self.notice = Some((error, false));
                    }
                    if let Some(folder) = &self.selected
                        && secondary(ui, "Copy model paths", true).clicked()
                    {
                        ui.ctx().copy_text(crate::model_paths(folder));
                    }
                });
                caption(ui, if std::env::var_os("HF_TOKEN").is_some_and(|value| !value.is_empty()) {
                    "Optional HF_TOKEN is available to this app."
                } else {
                    "Public downloads need no sign-in. For gated access, accept the model terms on Hugging Face and provide an optional read token through HF_TOKEN."
                });
                ui.hyperlink_to("Hugging Face token setup", "https://huggingface.co/docs/huggingface_hub/en/package_reference/environment_variables#hftoken");
            });
        }
    }

    fn show_pack_details(&mut self, ui: &mut Ui, index: usize, workspace: bool) {
        let mut selection = None;
        let mut verify_pack = None;
        let idle = !self.downloading() && self.scan.is_none();
        if let Some(pipeline) = DOWNLOAD_PIPELINES.get(index) {
            let library = self.library.as_ref();
            let missing: Vec<_> = pipeline
                .files
                .iter()
                .filter(|file| library.is_none_or(|library| !library.has(file)))
                .cloned()
                .collect();
            let missing_bytes: u64 = missing.iter().map(|file| file.size).sum();
            let targets = match (&self.connection, library) {
                (Some(connection), Some(library)) if !missing.is_empty() => {
                    if missing.iter().all(|file| library.reports_category(file)) {
                        missing
                            .iter()
                            .map(|file| {
                                let directory = library.destination(
                                    self.selected.as_deref(),
                                    &connection.default_root,
                                    file,
                                )?;
                                Ok(Target::new(file.clone(), directory.join(file.filename)))
                            })
                            .collect::<Result<Vec<_>, String>>()
                    } else {
                        Err("This build did not report all model categories for this pack. Refresh, or update ComfyUI if the category is unsupported.".into())
                    }
                }
                _ => Err("Start ComfyUI and refresh to choose a download location.".into()),
            };
            card(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if workspace {
                    theme::heading(ui, pipeline.name, 23.0);
                    let width = ui.available_width();
                    crate::artwork::pack_image(
                        ui,
                        pipeline.name,
                        egui::vec2(width, (width * 0.52).clamp(100.0, 160.0)),
                    );
                    let (label, color) = pack_status(library, pipeline);
                    ui.label(theme::strong(label, 13.0).color(theme::color(ui.ctx(), color)));
                    if pipeline.name == "Wan Animate 2"
                        && primary(ui, "Use in pipeline", true).clicked()
                    {
                        self.pipeline_requested = true;
                    }
                } else {
                    ui.horizontal_top(|ui| {
                        if pipeline.name == "Z-Image Turbo" && ui.available_width() >= 196.0 {
                            crate::artwork::starter_image(ui, egui::vec2(54.0, 38.0));
                        }
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            theme::heading(ui, pipeline.name, 18.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(pipeline.license)
                                        .size(12.0)
                                        .color(theme::color(ui.ctx(), MUTED)),
                                )
                                .wrap(),
                            );
                        });
                    });
                }
                if missing_bytes == 0 {
                    if !workspace {
                        caption(ui, "All files appear in the active library.");
                    }
                } else if let Ok(targets) = &targets {
                    let directories = targets
                        .iter()
                        .filter_map(|target| target.path.parent())
                        .collect::<BTreeSet<_>>();
                    caption(
                        ui,
                        &format!(
                            "{} · {} destination folder{}",
                            file_size(missing_bytes),
                            directories.len(),
                            if directories.len() == 1 { "" } else { "s" }
                        ),
                    );
                    let show_destinations = |ui: &mut Ui| {
                        ui.collapsing("Show download destinations", |ui| {
                            for folder in &directories {
                                crate::code_label(ui, &folder.to_string_lossy());
                            }
                        });
                    };
                    if workspace {
                        show_destinations(ui);
                    }
                    if library.is_some_and(|library| !library.paths_reported()) {
                        caption(
                            ui,
                            if workspace {
                                "Confirm this is the model folder used by the active ComfyUI build."
                            } else {
                                "Review the selected model folders below."
                            },
                        );
                    }
                    if primary(ui, "Download missing files", idle).clicked() {
                        selection = Some((pipeline.name, targets.clone()));
                    }
                    if !workspace {
                        show_destinations(ui);
                    }
                } else if let Err(error) = &targets {
                    caption(ui, error);
                }
                if library
                    .is_some_and(|library| pipeline.files.iter().any(|file| library.has(file)))
                    && secondary(ui, "Verify installed files", idle).clicked()
                {
                    verify_pack = Some(pipeline);
                }
                if !workspace {
                    ui.collapsing("Pack details", |ui| caption(ui, pipeline.description));
                }
                if let Some((name, report)) = &self.verification
                    && *name == pipeline.name
                {
                    caption(
                        ui,
                        &format!("{} installed file(s) matched the catalog.", report.verified),
                    );
                    if !report.repair.is_empty() {
                        caption(
                            ui,
                            &format!(
                                "{} file(s) differ from the pinned downloads. Manual variants can differ.",
                                report.repair.len()
                            ),
                        );
                        if self.confirm_repair {
                            caption(
                                ui,
                                "Repair downloads the catalog copies and keeps the originals as backups.",
                            );
                            for target in &report.repair {
                                caption(ui, &target.path.display().to_string());
                            }
                            let bytes = report.repair.iter().map(Target::bytes).sum();
                            ui.horizontal_wrapped(|ui| {
                                if primary(ui, &format!("Repair ({})", file_size(bytes)), idle)
                                    .clicked()
                                {
                                    selection = Some((pipeline.name, report.repair.clone()));
                                }
                                if secondary(ui, "Cancel", idle).clicked() {
                                    self.confirm_repair = false;
                                }
                            });
                        } else if secondary(ui, "Review repair", idle).clicked() {
                            self.confirm_repair = true;
                        }
                    }
                }
            });
        }
        if let Some((name, targets)) = selection {
            self.start_work(name, targets, false, ui.ctx());
            if workspace && self.downloading() {
                self.workspace_tab = ModelTab::Downloads;
            }
        } else if let Some(pipeline) = verify_pack
            && let (Some(connection), Some(library)) = (&self.connection, &self.library)
        {
            let targets = pipeline
                .files
                .iter()
                .filter(|file| library.has(file))
                .map(|file| {
                    library
                        .installed_path(self.selected.as_deref(), &connection.default_root, file)?
                        .map(|path| Target::new(file.clone(), path))
                        .ok_or(
                            "Refresh the active model library and try verifying again.".to_owned(),
                        )
                })
                .collect::<Result<Vec<_>, String>>();
            match targets {
                Ok(targets) => self.start_work(pipeline.name, targets, true, ui.ctx()),
                Err(error) => self.notice = Some((error, false)),
            }
        }
        ui.hyperlink_to("Model and pipeline guide", MODEL_GUIDE);
    }
}

const FILE_ROW_HEIGHT: f32 = 54.0;

#[derive(Clone, PartialEq, Eq)]
enum ModelSelection {
    Pack(usize),
    File { category: String, name: String },
}

#[derive(Clone, Copy)]
enum ModelGlyph {
    Layers,
    Package,
    Download,
    Image,
    Type,
    Sliders,
    Search,
    File,
}

fn model_views() -> [(ModelTab, &'static str, ModelGlyph); 3] {
    [
        (ModelTab::Installed, "Installed", ModelGlyph::Layers),
        (ModelTab::Packs, "Packs", ModelGlyph::Package),
        (ModelTab::Downloads, "Downloads", ModelGlyph::Download),
    ]
}

fn pane(ui: &mut Ui, width: f32, contents: impl FnOnce(&mut Ui)) {
    // The outer scrolling UI has no height limit. Give each nested pane
    // a viewport so its own scroll areas can use the available space.
    let height = ui.clip_rect().height().max(0.0);
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        Layout::top_down(Align::Min),
        |ui| {
            ui.set_width(width);
            contents(ui);
        },
    );
}

fn categories(library: Option<&Library>, tab: ModelTab) -> Vec<String> {
    let values: BTreeSet<_> = if tab == ModelTab::Packs {
        DOWNLOAD_PIPELINES
            .iter()
            .flat_map(|pack| {
                pack.files
                    .iter()
                    .map(|file| model_category(file).to_owned())
            })
            .collect()
    } else {
        library
            .into_iter()
            .flat_map(|library| {
                library
                    .files
                    .iter()
                    .filter(|(_, names)| !names.is_empty())
                    .map(|(category, _)| category.clone())
            })
            .collect()
    };
    let preferred = [
        "diffusion_models",
        "checkpoints",
        "text_encoders",
        "vae",
        "loras",
        "clip_vision",
        "background_removal",
    ];
    preferred
        .iter()
        .filter(|&name| values.contains(*name))
        .map(|name| (*name).to_owned())
        .chain(
            values
                .iter()
                .filter(|name| !preferred.contains(&name.as_str()))
                .cloned(),
        )
        .collect()
}

fn model_category(file: &ModelFile) -> &str {
    let path = file.folder.trim_end_matches('/');
    path.strip_prefix("models/").unwrap_or(path)
}

fn category_glyph(category: &str) -> ModelGlyph {
    match category {
        "diffusion_models" | "clip_vision" => ModelGlyph::Image,
        "text_encoders" => ModelGlyph::Type,
        "loras" => ModelGlyph::Sliders,
        "checkpoints" | "vae" => ModelGlyph::Layers,
        _ => ModelGlyph::File,
    }
}

fn filename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn pack_type(name: &str) -> &'static str {
    match name {
        "Wan Animate 2" => "Video  ·  Animate",
        "BiRefNet" => "Image  ·  Remove background",
        _ => "Image  ·  Generate",
    }
}

fn pack_status(library: Option<&Library>, pack: &Pipeline) -> (String, Color32) {
    let Some(library) = library else {
        return ("Availability unconfirmed".into(), MUTED);
    };
    let count = pack.files.iter().filter(|file| library.has(file)).count();
    if count == pack.files.len() {
        ("Installed".into(), GREEN)
    } else if count > 0 {
        (
            format!("{count} of {} installed", pack.files.len()),
            CAUTION,
        )
    } else if pack.files.iter().all(|file| library.reports_category(file)) {
        ("Available to download".into(), CAUTION)
    } else {
        ("Availability unconfirmed".into(), MUTED)
    }
}

fn transfer_ratio(done: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

fn card_action(ui: &mut Ui, heading: &str, detail: &str, action: &str, enabled: bool) -> bool {
    let mut clicked = false;
    card(ui, |ui| {
        ui.heading(heading);
        caption(ui, detail);
        clicked = secondary(ui, action, enabled).clicked();
    });
    clicked
}

fn nav_row(ui: &mut Ui, label: &str, glyph: ModelGlyph, selected: bool) -> egui::Response {
    let (bounds, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    let fill = if selected {
        theme::color(ui.ctx(), RAIL_ACTIVE)
    } else if response.hovered() {
        theme::color(ui.ctx(), CREAM)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT || response.has_focus() {
        let outline = if response.has_focus() || selected {
            Stroke::new(1.0_f32, theme::color(ui.ctx(), VIOLET))
        } else {
            Stroke::NONE
        };
        ui.painter()
            .rect(bounds, 7, fill, outline, egui::StrokeKind::Inside);
    }
    let text = theme::color(ui.ctx(), INK);
    draw_glyph(
        ui.painter(),
        egui::pos2(bounds.left() + 14.5, bounds.center().y),
        glyph,
        if selected {
            theme::color(ui.ctx(), VIOLET)
        } else {
            text
        },
    );
    let text_area = Rect::from_min_max(
        pos2(bounds.left() + 28.0, bounds.top()),
        bounds.right_bottom() - egui::vec2(5.0, 0.0),
    );
    paint_single(ui, text_area, theme::strong(label, 13.0), text);
    response.on_hover_text(label)
}

enum RowPreview<'a> {
    Pack(&'a str),
    Glyph(ModelGlyph),
}

fn card_row(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    status: Option<(&str, Color32)>,
    selected: bool,
    preview: RowPreview<'_>,
) -> egui::Response {
    let pack = matches!(preview, RowPreview::Pack(_));
    let height = if pack { 102.0 } else { FILE_ROW_HEIGHT };
    let (bounds, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    let accessibility = status.map_or_else(
        || format!("{title}. {subtitle}"),
        |(label, _)| format!("{title}. {subtitle}. {label}"),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            &accessibility,
        )
    });
    let fill = if selected { RAIL_ACTIVE } else { SURFACE };
    let highlighted = selected || response.hovered() || response.has_focus();
    let border = theme::color(ui.ctx(), if highlighted { VIOLET } else { LINE });
    ui.painter().rect(
        bounds,
        10,
        theme::color(ui.ctx(), fill),
        Stroke::new(if selected { 1.8_f32 } else { 1.0_f32 }, border),
        egui::StrokeKind::Inside,
    );
    let area = bounds.shrink(9.0);
    let preview_width = if pack {
        (bounds.width() * 0.31).clamp(60.0, 132.0)
    } else {
        35.0
    };
    let preview_bounds = Rect::from_min_size(area.min, egui::vec2(preview_width, area.height()));
    let painted_preview = if let RowPreview::Pack(name) = preview {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((response.id, "model-art"))
                .max_rect(preview_bounds)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_clip_rect(preview_bounds.intersect(ui.clip_rect()));
        crate::artwork::pack_image(&mut child, name, preview_bounds.size())
    } else {
        false
    };
    if !painted_preview {
        ui.painter()
            .rect_filled(preview_bounds, 7, theme::color(ui.ctx(), CREAM));
        let glyph = match preview {
            RowPreview::Pack(_) => ModelGlyph::Package,
            RowPreview::Glyph(glyph) => glyph,
        };
        draw_glyph(
            ui.painter(),
            preview_bounds.center(),
            glyph,
            theme::color(ui.ctx(), VIOLET),
        );
    }

    let x = preview_bounds.right() + 10.0;
    let right = area.right() - if pack { 2.0 } else { 12.0 };
    let row = |top: f32, height: f32| {
        Rect::from_min_max(
            pos2(x, area.top() + top),
            pos2(right, area.top() + top + height),
        )
    };
    paint_single(
        ui,
        row(if pack { 6.0 } else { 0.0 }, if pack { 25.0 } else { 21.0 }),
        theme::strong(title, if pack { 17.0 } else { 13.5 }),
        theme::color(ui.ctx(), INK),
    );
    paint_single(
        ui,
        row(if pack { 34.0 } else { 20.0 }, 17.0),
        RichText::new(subtitle).size(12.0),
        theme::color(ui.ctx(), MUTED),
    );
    if let Some((label, role)) = status {
        let y = area.top() + 64.0;
        let status_color = theme::color(ui.ctx(), role);
        if role == GREEN && right - x >= 74.0 {
            let badge = Rect::from_min_size(pos2(x, y - 1.0), egui::vec2(74.0, 18.0));
            let foreground = theme::on_fill(status_color);
            ui.painter().rect_filled(badge, 5, status_color);
            ui.painter().line(
                vec![
                    pos2(x + 7.0, y + 8.0),
                    pos2(x + 10.0, y + 11.0),
                    pos2(x + 15.0, y + 5.0),
                ],
                Stroke::new(1.4_f32, foreground),
            );
            paint_single(
                ui,
                Rect::from_min_max(pos2(x + 20.0, badge.top()), badge.max),
                theme::strong(label, 11.5),
                foreground,
            );
        } else {
            let bar = Rect::from_min_max(pos2(x, y), pos2(right, y + 16.0));
            ui.painter()
                .circle_filled(pos2(x + 3.0, bar.center().y), 2.6, status_color);
            paint_single(
                ui,
                Rect::from_min_max(pos2(x + 10.0, y), bar.max),
                theme::strong(label, 11.5),
                status_color,
            );
        }
    }
    if !pack {
        let c = pos2(area.right() - 4.0, area.center().y);
        let s = Stroke::new(1.5_f32, theme::color(ui.ctx(), MUTED));
        ui.painter().line(
            vec![
                c + egui::vec2(-2.5, -4.0),
                c + egui::vec2(1.5, 0.0),
                c + egui::vec2(-2.5, 4.0),
            ],
            s,
        );
    }
    response
}

fn paint_single(ui: &Ui, bounds: Rect, text: RichText, fallback: Color32) {
    if !bounds.is_positive() {
        return;
    }
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        bounds.width(),
        egui::FontSelection::Default,
    );
    let position = pos2(bounds.left(), bounds.center().y - galley.size().y * 0.5);
    ui.painter()
        .with_clip_rect(bounds)
        .galley(position, galley, fallback);
}

fn draw_glyph(painter: &egui::Painter, center: egui::Pos2, glyph: ModelGlyph, color: Color32) {
    let s = Stroke::new(1.45_f32, color);
    let p = |x, y| center + egui::vec2(x, y);
    let line =
        |points: &[(f32, f32)]| painter.line(points.iter().map(|&(x, y)| p(x, y)).collect(), s);
    match glyph {
        ModelGlyph::Layers => {
            line(&[
                (-7.0, -2.5),
                (0.0, -6.2),
                (7.0, -2.5),
                (0.0, 1.3),
                (-7.0, -2.5),
            ]);
            line(&[(-7.0, 1.1), (0.0, 4.8), (7.0, 1.1)]);
            line(&[(-7.0, 4.5), (0.0, 8.0), (7.0, 4.5)]);
        }
        ModelGlyph::Package => {
            line(&[
                (-6.2, -3.4),
                (0.0, -7.0),
                (6.2, -3.4),
                (6.2, 3.6),
                (0.0, 7.0),
                (-6.2, 3.6),
                (-6.2, -3.4),
                (0.0, 0.2),
                (6.2, -3.4),
            ]);
            line(&[(0.0, 0.2), (0.0, 7.0)]);
        }
        ModelGlyph::Download => {
            line(&[(0.0, -7.0), (0.0, 2.7)]);
            line(&[(-3.5, -0.8), (0.0, 2.7), (3.5, -0.8)]);
            line(&[(-6.5, 3.5), (-6.5, 6.5), (6.5, 6.5), (6.5, 3.5)]);
        }
        ModelGlyph::Image => {
            painter.rect_stroke(
                Rect::from_min_max(p(-7.1, -5.7), p(7.1, 5.7)),
                1.4,
                s,
                egui::StrokeKind::Inside,
            );
            painter.circle_filled(p(-3.4, -2.1), 1.1, color);
            line(&[
                (-6.0, 4.5),
                (-1.1, 0.0),
                (1.3, 2.4),
                (4.0, -0.6),
                (6.1, 1.7),
            ]);
        }
        ModelGlyph::Type => {
            line(&[(-5.9, -4.8), (5.9, -4.8)]);
            line(&[(0.0, -4.8), (0.0, 5.9)]);
            line(&[(-2.6, 5.9), (2.6, 5.9)]);
        }
        ModelGlyph::Sliders => {
            line(&[(-6.5, -4.2), (6.5, -4.2)]);
            line(&[(-6.5, 1.0), (6.5, 1.0)]);
            line(&[(-6.5, 6.0), (6.5, 6.0)]);
            painter.circle_filled(p(-2.6, -4.2), 1.8, color);
            painter.circle_filled(p(3.1, 1.0), 1.8, color);
            painter.circle_filled(p(-0.4, 6.0), 1.8, color);
        }
        ModelGlyph::Search => {
            painter.circle_stroke(p(-1.1, -1.2), 4.7, s);
            line(&[(2.6, 2.3), (6.0, 5.9)]);
        }
        ModelGlyph::File => {
            line(&[
                (-4.9, -7.0),
                (2.2, -7.0),
                (5.2, -4.0),
                (5.2, 6.5),
                (-4.9, 6.5),
                (-4.9, -7.0),
            ]);
            line(&[(2.0, -6.6), (2.0, -3.7), (4.7, -3.7)]);
            line(&[(-2.5, 0.6), (2.5, 0.6)]);
            line(&[(-2.5, 3.5), (2.5, 3.5)]);
        }
    }
}

fn category_name(category: &str) -> String {
    match category {
        "diffusion_models" => "Diffusion models".into(),
        "text_encoders" => "Text encoders".into(),
        "clip_vision" => "Vision encoders".into(),
        "loras" => "LoRAs".into(),
        "vae" => "VAE".into(),
        _ => category.replace('_', " "),
    }
}

fn matches_installed_query(
    category: &str,
    filename: &str,
    selected_category: Option<&str>,
    query: &str,
) -> bool {
    selected_category.is_none_or(|selected| selected == category)
        && filename.to_lowercase().contains(query)
}

fn file_size(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_download_can_resume_only_for_the_same_destination() {
        let model = DOWNLOAD_PIPELINES[0].files[0].clone();
        for (initial, current, resumable) in [("first", "first", true), ("first", "other", false)] {
            let (tx, rx) = mpsc::channel();
            tx.send(Err(model_download::ABORTED.into())).unwrap();
            let mut manager = ModelManager {
                selected: Some(PathBuf::from(current)),
                transfer: Some(Transfer {
                    result: rx,
                    cancel: Arc::new(AtomicBool::new(true)),
                    bytes: Arc::new(AtomicU64::new(1)),
                    total: Arc::new(AtomicU64::new(2)),
                    name: "Fixture pack",
                    verifying: false,
                    resume: vec![Target::new(
                        model.clone(),
                        PathBuf::from(initial).join(model.filename),
                    )],
                    destination: Some(PathBuf::from(initial)),
                }),
                ..Default::default()
            };
            manager.update(&egui::Context::default());
            assert_eq!(manager.paused.is_some(), resumable);
            assert!(!manager.downloading());
        }
    }

    #[test]
    fn installed_view_keeps_custom_files_visible_and_does_not_guess_pack_status() {
        let pack = &DOWNLOAD_PIPELINES[2];
        let motion = &pack.files[0];
        let mut library = Library::default();
        let category = model_category(motion);
        library.files.insert(
            category.into(),
            vec![
                format!("custom/{}", motion.filename),
                "personal.safetensors".into(),
            ],
        );
        let manager = ModelManager {
            library: Some(library),
            ..Default::default()
        };
        assert!(
            manager
                .workspace_items(ModelTab::Installed, Some(category), "wan")
                .contains(&ModelSelection::Pack(2))
        );
        assert!(
            manager
                .workspace_items(ModelTab::Installed, Some(category), "personal")
                .contains(&ModelSelection::File {
                    category: category.into(),
                    name: "personal.safetensors".into()
                })
        );
        assert!(
            manager
                .workspace_items(ModelTab::Installed, Some("vae"), "wan")
                .is_empty()
        );
        assert_ne!(pack_status(manager.library.as_ref(), pack).0, "Installed");
    }

    #[test]
    fn filters_installed_paths_and_requires_the_selected_category() {
        let file = "motion/Wan_Animate.safetensors";
        assert!(matches_installed_query(
            "diffusion_models",
            file,
            None,
            "wan"
        ));
        assert!(matches_installed_query(
            "diffusion_models",
            file,
            Some("diffusion_models"),
            "motion/wan",
        ));
        assert!(!matches_installed_query(
            "diffusion_models",
            file,
            Some("loras"),
            "wan",
        ));
        assert!(!matches_installed_query(
            "diffusion_models",
            file,
            None,
            "vae"
        ));
        assert!(matches_installed_query(
            "diffusion_models",
            file,
            Some("diffusion_models"),
            "",
        ));
    }
}
