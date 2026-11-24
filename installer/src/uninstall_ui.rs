use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui;

use crate::utility_ui::{self, UtilityIcon};
use crate::{
    CAUTION, activity, caption, code_label, destructive, field_label, panel_modal, secondary,
    status, uninstall,
};

#[derive(Default)]
pub(crate) struct UninstallDialog {
    state: Option<State>,
}

enum State {
    Confirm(PathBuf),
    Removing(Receiver<Result<(), String>>),
    Finished(Result<(), String>),
}

impl UninstallDialog {
    pub(crate) fn choose(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a Render Bridge installation to uninstall")
            .pick_folder()
        {
            self.state = Some(match uninstall::inspect(&folder) {
                Ok(()) => State::Confirm(folder),
                Err(message) => State::Finished(Err(message)),
            });
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self.state, Some(State::Removing(_)))
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) {
        let Some(state) = self.state.take() else {
            return;
        };
        let state = match state {
            State::Removing(receiver) => match receiver.try_recv() {
                Ok(result) => State::Finished(result),
                Err(TryRecvError::Disconnected) => State::Finished(Err(
                    "Uninstall did not finish. Open the installation again to retry.".into(),
                )),
                Err(TryRecvError::Empty) => State::Removing(receiver),
            },
            other => other,
        };
        let response = panel_modal(ctx, "uninstall-render-bridge").show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(120.0, 394.0));
            let detail = match &state {
                State::Confirm(_) => "Review the selected installation",
                State::Removing(_) => "Removing program files",
                State::Finished(Ok(())) => "Program files removed",
                State::Finished(Err(_)) => "Uninstall could not finish",
            };
            utility_ui::heading(ui, UtilityIcon::Maintenance, "Uninstall Render Bridge", Some(detail));
            utility_ui::scroll_body(ui, "uninstall-dialog-body", |ui| match &state {
                State::Confirm(folder) => {
                    field_label(ui, "Installation folder");
                    code_label(ui, &folder.to_string_lossy());
                    ui.separator();
                    caption(ui, "Exit Render Bridge from the tray and stop its server first. Uninstall removes verified program files and matching shortcuts. Your saved settings, token, project records, and ComfyUI assets stay on disk.");
                }
                State::Removing(_) => activity(ui, "Removing the selected installation…"),
                State::Finished(Ok(())) => caption(ui, "Your saved data is available for reinstalling."),
                State::Finished(Err(message)) => status(ui, "Check the installation", message, CAUTION),
            });
            utility_ui::footer(ui);
            match &state {
                State::Confirm(_) => ui
                    .horizontal_wrapped(|ui| {
                        let cancel = secondary(ui, "Cancel", true).clicked();
                        let confirm = destructive(ui, "Uninstall", true).clicked();
                        (cancel, confirm)
                    })
                    .inner,
                State::Removing(_) => (false, false),
                State::Finished(_) => (secondary(ui, "Done", true).clicked(), false),
            }
        });
        self.state = match state {
            State::Confirm(folder) if response.inner.1 => Some(start(ctx, folder)),
            State::Removing(_) => Some(state),
            _ if response.inner.0 || response.should_close() => None,
            _ => Some(state),
        };
    }
}

fn start(ctx: &egui::Context, folder: PathBuf) -> State {
    let (sender, receiver) = mpsc::channel();
    let ctx = ctx.clone();
    match thread::Builder::new()
        .name("uninstall-render-bridge".into())
        .spawn(move || {
            let _ = sender.send(uninstall::uninstall(&folder));
            ctx.request_repaint();
        }) {
        Ok(_) => State::Removing(receiver),
        Err(_) => State::Finished(Err("Could not start the uninstaller. Try again.".into())),
    }
}
