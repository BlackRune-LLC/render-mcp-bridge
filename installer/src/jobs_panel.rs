use eframe::egui::{self, RichText, Ui};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::jobs::{self, QueueStatus};
use crate::theme;
use crate::{CAUTION, GREEN, INK, LINE, SURFACE, VIOLET, caption, monitor_button, status};

#[cfg(windows)]
use crate::RAIL_ACTIVE;
#[cfg(windows)]
use crate::job_progress::Observer;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const FRESH_FOR: Duration = Duration::from_secs(3);
const CHECK_ERROR: &str = "The queue check could not start. Try Refresh.";

type Snapshot = Arc<Result<QueueStatus, String>>;

#[derive(Default)]
struct QueueState {
    snapshot: Option<Snapshot>,
    checked_at: Option<Instant>,
    checking: bool,
    manual: bool,
    stop_waiting: bool,
}

impl QueueState {
    fn finish(&mut self, result: Result<QueueStatus, String>, explicit: bool) -> bool {
        self.checking = false;
        self.checked_at = Some(Instant::now());
        if explicit {
            self.manual = false;
        }
        let changed = self.snapshot.as_deref() != Some(&result);
        if changed {
            self.snapshot = Some(Arc::new(result));
        }
        // Wake an open stop dialog if it observed an in-flight check.
        let stop_changed = !self.manual && std::mem::take(&mut self.stop_waiting);
        changed || explicit || stop_changed
    }
}

pub struct QueueView {
    url: String,
    state: Arc<Mutex<QueueState>>,
    refresh: Option<Sender<()>>,
    #[cfg(windows)]
    progress: Option<Observer>,
}

impl QueueView {
    pub fn new(url: String) -> Self {
        Self {
            url,
            state: Arc::new(Mutex::new(QueueState::default())),
            refresh: None,
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

    #[cfg(windows)]
    pub fn has_active_work(&self) -> bool {
        self.state.lock().is_ok_and(
            |state| matches!(state.snapshot.as_deref(), Some(Ok(queue)) if !queue.is_idle()),
        )
    }

    fn start_worker(&mut self, ctx: &egui::Context, mut explicit: bool) {
        let (refresh, requested) = mpsc::channel();
        let url = self.url.clone();
        let ctx = ctx.clone();
        let state = Arc::clone(&self.state);
        match thread::Builder::new()
            .name("check-comfyui-jobs".into())
            .spawn(move || {
                loop {
                    let Ok(mut snapshot) = state.lock() else {
                        break;
                    };
                    snapshot.checking = true;
                    drop(snapshot);
                    let result = jobs::check(&url);
                    let changed = state
                        .lock()
                        .is_ok_and(|mut state| state.finish(result, explicit));
                    if changed {
                        ctx.request_repaint();
                    }
                    explicit = match requested.recv_timeout(REFRESH_INTERVAL) {
                        Ok(()) => true,
                        Err(RecvTimeoutError::Timeout) => false,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                }
            }) {
            Ok(_) => self.refresh = Some(refresh),
            Err(_) => {
                if let Ok(mut state) = self.state.lock() {
                    state.finish(Err(CHECK_ERROR.into()), true);
                }
            }
        }
    }

    fn request_refresh(&mut self, ctx: &egui::Context) {
        if let Some(refresh) = &self.refresh {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if refresh.send(()).is_ok() {
                state.manual = true;
                return;
            }
        }
        if let Ok(mut state) = self.state.lock() {
            state.manual = true;
        }
        self.start_worker(ctx, true);
    }

    pub fn show(&mut self, ui: &mut Ui) {
        self.show_card(ui);
        ui.horizontal_wrapped(|ui| self.show_refresh(ui));
    }

    pub fn show_card(&mut self, ui: &mut Ui) {
        let mut snapshot = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.snapshot.clone());
        if self.refresh.is_none() && snapshot.is_none() {
            self.start_worker(ui.ctx(), false);
            snapshot = self
                .state
                .lock()
                .ok()
                .and_then(|state| state.snapshot.clone());
        }
        match snapshot.as_deref() {
            Some(Ok(queue)) => {
                egui::Frame::new()
                    .fill(theme::color(ui.ctx(), SURFACE))
                    .stroke(egui::Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
                    .corner_radius(12)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let (label, color) = match (queue.running > 0, queue.queued > 0) {
                            (true, _) => ("Render in progress", GREEN),
                            (false, true) => ("Waiting to run", VIOLET),
                            _ => ("Queue is idle", GREEN),
                        };
                        ui.label(
                            RichText::new(label)
                                .size(17.0)
                                .strong()
                                .color(theme::color(ui.ctx(), color)),
                        );
                        if queue.running > 0 {
                            self.show_progress(ui, queue);
                            ui.separator();
                        }
                        ui.horizontal_wrapped(|ui| {
                            for (label, count) in
                                [("running", queue.running), ("queued", queue.queued)]
                            {
                                ui.label(
                                    RichText::new(format!("{count} {label}"))
                                        .strong()
                                        .color(theme::color(ui.ctx(), INK)),
                                );
                            }
                        });
                    });
            }
            Some(Err(message)) => status(ui, "Queue status unavailable", message, CAUTION),
            None => caption(ui, "Checking the ComfyUI queue…"),
        }
    }

    pub fn show_refresh(&mut self, ui: &mut Ui) {
        let (available, mut manual) = self
            .state
            .lock()
            .map(|state| (state.snapshot.is_some(), state.manual))
            .unwrap_or_default();
        if monitor_button(ui, "Refresh", available && !manual, SURFACE)
            .on_hover_text("The queue also refreshes automatically")
            .clicked()
        {
            self.request_refresh(ui.ctx());
            manual = self.state.lock().is_ok_and(|state| state.manual);
        }
        if manual {
            caption(ui, "Refreshing queue…");
        }
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
                            .color(theme::color(ui.ctx(), GREEN)),
                    );
                });
            });
            ui.scope(|ui| {
                ui.visuals_mut().extreme_bg_color = theme::color(ui.ctx(), RAIL_ACTIVE);
                ui.add(
                    egui::ProgressBar::new(fraction as f32)
                        .desired_height(10.0)
                        .fill(theme::color(ui.ctx(), GREEN)),
                );
            });
            return;
        }

        caption(ui, "Step updates appear when available.");
    }

    pub fn stop_confirmation(
        &self,
        stop: &'static str,
        stop_anyway: &'static str,
    ) -> Option<&'static str> {
        let mut state = self.state.lock().ok()?;
        if state.checking
            || state.manual
            || state
                .checked_at
                .is_none_or(|checked| checked.elapsed() > FRESH_FOR)
        {
            state.stop_waiting = true;
            return None;
        }
        state.snapshot.as_deref().map(|result| {
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
        let view = QueueView::new("http://127.0.0.1:8188".into());
        let confirmation =
            |view: &QueueView| view.stop_confirmation("Stop Bridge", "Stop Bridge anyway");
        assert_eq!(confirmation(&view), None);
        view.state.lock().unwrap().finish(Ok(queue(0)), false);
        assert_eq!(confirmation(&view), Some("Stop Bridge"));
        view.state.lock().unwrap().checking = true;
        assert_eq!(confirmation(&view), None);
        assert!(view.state.lock().unwrap().finish(Ok(queue(0)), false));
        view.state.lock().unwrap().manual = true;
        assert_eq!(confirmation(&view), None);
        view.state.lock().unwrap().finish(Ok(queue(2)), true);
        assert_eq!(confirmation(&view), Some("Stop Bridge anyway"));
        view.state
            .lock()
            .unwrap()
            .finish(Err("unavailable".into()), false);
        assert_eq!(confirmation(&view), Some("Stop Bridge anyway"));
        view.state.lock().unwrap().checked_at = Some(Instant::now() - FRESH_FOR * 2);
        assert_eq!(confirmation(&view), None);
    }

    #[test]
    fn unchanged_automatic_checks_keep_the_card_and_manual_checks_acknowledge_completion() {
        let mut state = QueueState::default();
        assert!(state.finish(Ok(queue(0)), false));
        let original = state.snapshot.clone().unwrap();
        assert!(!state.finish(Ok(queue(0)), false));
        assert!(Arc::ptr_eq(&original, state.snapshot.as_ref().unwrap()));
        state.manual = true;
        assert!(state.finish(Ok(queue(0)), true));
        assert!(!state.manual);
        assert!(Arc::ptr_eq(&original, state.snapshot.as_ref().unwrap()));
        assert!(state.finish(Ok(queue(2)), false));
    }

    fn queue(queued: usize) -> QueueStatus {
        QueueStatus {
            running: 0,
            queued,
            running_ids: Vec::new(),
        }
    }
}
