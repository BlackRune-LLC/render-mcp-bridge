use eframe::egui::{self, RichText, Ui};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::jobs::{self, QueueStatus};
use crate::{CAUTION, GREEN, INK, MUTED, VIOLET, activity, caption, card, secondary, status};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub struct QueueView {
    url: String,
    result: Option<Result<QueueStatus, String>>,
    worker: Option<Receiver<Result<QueueStatus, String>>>,
    next_check: Instant,
}

impl QueueView {
    pub fn new(url: String) -> Self {
        Self {
            url,
            result: None,
            worker: None,
            next_check: Instant::now(),
        }
    }

    pub fn is_for(&self, url: &str) -> bool {
        self.url == url
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let url = self.url.clone();
        let ctx = ctx.clone();
        match thread::Builder::new()
            .name("check-comfyui-jobs".into())
            .spawn(move || {
                let _ = sender.send(jobs::check(&url));
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(receiver),
            Err(_) => self.finish(Err("The queue check could not start. Try Refresh.".into())),
        }
    }

    fn finish(&mut self, result: Result<QueueStatus, String>) {
        self.worker = None;
        self.result = Some(result);
        self.next_check = Instant::now() + REFRESH_INTERVAL;
    }

    pub fn show(&mut self, ui: &mut Ui) {
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(result) => self.finish(result),
                Err(TryRecvError::Disconnected) => {
                    self.finish(Err("The queue check did not finish. Try Refresh.".into()));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.worker.is_none() && Instant::now() >= self.next_check {
            self.refresh(ui.ctx());
        }
        if self.worker.is_none() {
            ui.ctx()
                .request_repaint_after(self.next_check.saturating_duration_since(Instant::now()));
        }
        match &self.result {
            Some(Ok(queue)) => {
                card(ui, |ui| {
                    let (label, color) = match (self.worker.is_some(), queue.is_idle()) {
                        (true, true) => ("Last check was idle", MUTED),
                        (true, false) => ("Work was in progress", VIOLET),
                        (false, true) => ("ComfyUI is idle", GREEN),
                        (false, false) => ("Work in progress", VIOLET),
                    };
                    ui.label(RichText::new(label).size(17.0).strong().color(color));
                    ui.columns(2, |columns| {
                        for (column, label, count) in
                            [(0, "Running", queue.running), (1, "Queued", queue.queued)]
                        {
                            columns[column].label(
                                RichText::new(count.to_string())
                                    .size(29.0)
                                    .strong()
                                    .color(INK),
                            );
                            columns[column].label(RichText::new(label).color(MUTED));
                        }
                    });
                    if !queue.is_idle() {
                        activity(ui, "Watching the active queue");
                    }
                });
            }
            Some(Err(message)) => status(ui, "Queue status unavailable", message, CAUTION),
            None => activity(ui, "Checking the ComfyUI queue…"),
        }
        ui.horizontal_wrapped(|ui| {
            if secondary(ui, "Refresh", self.worker.is_none()).clicked() {
                self.refresh(ui.ctx());
            }
            caption(
                ui,
                if self.worker.is_some() {
                    "Checking for updates"
                } else {
                    "Updates automatically"
                },
            );
        });
    }

    pub fn stop_confirmation(&self) -> Option<&'static str> {
        if self.worker.is_some() {
            return None;
        }
        self.result.as_ref().map(|result| {
            if result.as_ref().is_ok_and(QueueStatus::is_idle) {
                "Stop Bridge"
            } else {
                "Stop Bridge anyway"
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_observed_idle_queue_gets_the_ordinary_stop_label() {
        let mut view = QueueView::new("http://127.0.0.1:8188".into());
        assert_eq!(view.stop_confirmation(), None);
        view.finish(Ok(QueueStatus {
            running: 0,
            queued: 0,
        }));
        assert_eq!(view.stop_confirmation(), Some("Stop Bridge"));
        let (_sender, pending) = mpsc::channel();
        view.worker = Some(pending);
        assert_eq!(view.stop_confirmation(), None);
        view.finish(Ok(QueueStatus {
            running: 0,
            queued: 2,
        }));
        assert_eq!(view.stop_confirmation(), Some("Stop Bridge anyway"));
        view.finish(Err("unavailable".into()));
        assert_eq!(view.stop_confirmation(), Some("Stop Bridge anyway"));
    }
}
