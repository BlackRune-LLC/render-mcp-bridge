use eframe::egui::{self, RichText, Ui};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crate::jobs::{self, QueueStatus};
use crate::{
    CAUTION, GREEN, INK, MUTED, RAIL_ACTIVE, SURFACE, VIOLET, activity, caption, card,
    monitor_button, status,
};

#[cfg(windows)]
use crate::job_progress::Observer;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub struct QueueView {
    url: String,
    result: Option<Result<QueueStatus, String>>,
    worker: Option<Receiver<Result<QueueStatus, String>>>,
    next_check: Instant,
    #[cfg(windows)]
    progress: Option<Observer>,
}

impl QueueView {
    pub fn new(url: String) -> Self {
        Self {
            url,
            result: None,
            worker: None,
            next_check: Instant::now(),
            #[cfg(windows)]
            progress: None,
        }
    }

    pub fn with_progress(url: String, ctx: &egui::Context) -> Self {
        #[cfg(windows)]
        return Self {
            progress: Observer::start(&url, ctx.clone()),
            ..Self::new(url)
        };
        #[cfg(not(windows))]
        {
            let _ = ctx;
            Self::new(url)
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
                    let (label, color) =
                        match (self.worker.is_some(), queue.running > 0, queue.queued > 0) {
                            (true, false, false) => ("Last check was idle", MUTED),
                            (_, true, _) => ("Work in progress", VIOLET),
                            (_, false, true) => ("Waiting to run", VIOLET),
                            _ => ("Queue is idle", GREEN),
                        };
                    ui.label(RichText::new(label).size(17.0).strong().color(color));
                    ui.horizontal_wrapped(|ui| {
                        for (label, count) in [("Running", queue.running), ("Queued", queue.queued)]
                        {
                            ui.label(
                                RichText::new(count.to_string())
                                    .size(19.0)
                                    .strong()
                                    .color(INK),
                            );
                            ui.label(RichText::new(label).color(MUTED));
                            ui.add_space(9.0);
                        }
                    });
                    if queue.running > 0 {
                        self.show_progress(ui, queue);
                    }
                });
            }
            Some(Err(message)) => status(ui, "Queue status unavailable", message, CAUTION),
            None => activity(ui, "Checking the ComfyUI queue…"),
        }
        ui.horizontal_wrapped(|ui| {
            if monitor_button(ui, "Refresh", self.worker.is_none(), SURFACE).clicked() {
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

    fn show_progress(&self, ui: &mut Ui, _queue: &QueueStatus) {
        #[cfg(windows)]
        if let Some(progress) = self.progress.as_ref().and_then(|observer| {
            _queue
                .running_ids
                .iter()
                .find_map(|id| observer.latest_for(id))
        }) {
            let fraction = progress.value / progress.max;
            let percent = if progress.value < progress.max {
                (fraction * 100.0).round().min(99.0)
            } else {
                100.0
            };
            ui.horizontal(|ui| {
                caption(
                    ui,
                    &format!("Current stage {:.0} of {:.0}", progress.value, progress.max),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{percent:.0}%"))
                            .strong()
                            .color(VIOLET),
                    );
                });
            });
            ui.scope(|ui| {
                ui.visuals_mut().extreme_bg_color = RAIL_ACTIVE;
                ui.add(
                    egui::ProgressBar::new(fraction as f32)
                        .desired_height(10.0)
                        .fill(VIOLET),
                );
            });
            return;
        }

        let (track, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), egui::Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::ProgressIndicator,
                ui.is_enabled(),
                "Job in progress",
            )
        });
        if ui.is_rect_visible(track) {
            let width = track.width() * 0.24;
            let phase = ui.input(|input| (input.time * 2.3).sin() * 0.5 + 0.5) as f32;
            let left = track.left() + (track.width() - width) * phase;
            let segment = egui::Rect::from_min_size(
                egui::pos2(left, track.top()),
                egui::vec2(width, track.height()),
            );
            ui.painter().rect_filled(track, 5, RAIL_ACTIVE);
            ui.painter().rect_filled(segment, 5, VIOLET);
            ui.ctx().request_repaint_after(Duration::from_millis(32));
        }
        caption(ui, "Step updates appear when available.");
    }

    pub fn stop_confirmation(
        &self,
        stop: &'static str,
        stop_anyway: &'static str,
    ) -> Option<&'static str> {
        if self.worker.is_some() {
            return None;
        }
        self.result.as_ref().map(|result| {
            if result.as_ref().is_ok_and(QueueStatus::is_idle) {
                stop
            } else {
                stop_anyway
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
        let confirmation =
            |view: &QueueView| view.stop_confirmation("Stop Bridge", "Stop Bridge anyway");
        assert_eq!(confirmation(&view), None);
        view.finish(Ok(QueueStatus {
            running: 0,
            queued: 0,
            running_ids: Vec::new(),
        }));
        assert_eq!(confirmation(&view), Some("Stop Bridge"));
        let (_sender, pending) = mpsc::channel();
        view.worker = Some(pending);
        assert_eq!(confirmation(&view), None);
        view.finish(Ok(QueueStatus {
            running: 0,
            queued: 2,
            running_ids: Vec::new(),
        }));
        assert_eq!(confirmation(&view), Some("Stop Bridge anyway"));
        view.finish(Err("unavailable".into()));
        assert_eq!(confirmation(&view), Some("Stop Bridge anyway"));
    }
}
