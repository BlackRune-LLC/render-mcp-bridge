use eframe::egui::{self, Ui};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::INK;
use crate::jobs::{self, QueueStatus};
use crate::theme;
use crate::{CAUTION, CREAM, GREEN, LINE, MUTED, SURFACE, VIOLET, caption, monitor_button, status};

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
        self.show_queue(ui, false);
    }

    pub fn show_card(&mut self, ui: &mut Ui) {
        self.show_queue(ui, true);
    }

    fn show_queue(&mut self, ui: &mut Ui, dashboard: bool) {
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
            Some(Ok(queue)) => self.queue_snapshot(ui, queue, dashboard),
            Some(Err(message)) => {
                status(ui, "Queue status unavailable", message, CAUTION);
                ui.horizontal_wrapped(|ui| self.show_refresh(ui));
            }
            None => queue_frame(ui, 12, |ui| caption(ui, "Checking the ComfyUI queue…")),
        }
        if self.state.lock().is_ok_and(|state| state.manual) {
            caption(ui, "Refreshing queue…");
        }
    }

    fn queue_snapshot(&mut self, ui: &mut Ui, queue: &QueueStatus, dashboard: bool) {
        let (title, tone, shape) = if queue.running > 0 {
            ("Render in progress", GREEN, QueueGlyph::Active)
        } else if queue.queued > 0 {
            ("Waiting to render", VIOLET, QueueGlyph::Layers)
        } else {
            ("Queue idle", GREEN, QueueGlyph::Ready)
        };
        queue_frame(ui, if dashboard { 10 } else { 12 }, |ui| {
            ui.horizontal_top(|ui| {
                if dashboard {
                    queue_glyph(ui, shape, tone, 49.0);
                    ui.add_space(5.0);
                }
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        theme::heading(ui, title, if dashboard { 19.0 } else { 16.0 });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            self.show_refresh(ui);
                        });
                    });
                    if queue.running > 0 {
                        self.show_progress(ui, queue);
                    }
                    ui.horizontal_wrapped(|ui| {
                        queue_count(ui, queue.running, "running", tone);
                        queue_count(ui, queue.queued, "queued", VIOLET);
                    });
                });
            });
        });
        if dashboard {
            for id in &queue.pending_ids {
                queue_frame(ui, 5, |ui| {
                    ui.horizontal(|ui| {
                        queue_glyph(ui, QueueGlyph::Layers, VIOLET, 22.0);
                        let short: String = id.chars().take(10).collect();
                        let title = theme::strong(format!("Job {short}"), 13.0);
                        ui.add(egui::Label::new(title).truncate()).on_hover_text(id);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            queue_tag(ui, "Queued", VIOLET);
                        });
                    });
                });
            }
            let unlisted = queue.queued.saturating_sub(queue.pending_ids.len());
            if unlisted > 0 && !queue.pending_ids.is_empty() {
                caption(ui, &format!("{unlisted} more queued"));
            }
        }
    }

    fn show_refresh(&mut self, ui: &mut Ui) {
        let (available, manual) = self
            .state
            .lock()
            .map(|state| (state.snapshot.is_some(), state.manual))
            .unwrap_or_default();
        if monitor_button(ui, "Refresh", available && !manual, SURFACE)
            .on_hover_text("Check the queue now. The queue also refreshes automatically.")
            .clicked()
        {
            self.request_refresh(ui.ctx());
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
            let fraction = (progress.value / progress.max).clamp(0.0, 1.0);
            let percent = if progress.value < progress.max {
                (fraction * 100.0).round().min(99.0)
            } else {
                100.0
            };
            ui.horizontal_wrapped(|ui| {
                caption(
                    ui,
                    &format!("Current stage {:.0} of {:.0}", progress.value, progress.max),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        theme::strong(format!("{percent:.0}%"), 17.0)
                            .color(theme::color(ui.ctx(), INK)),
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
        caption(ui, "Waiting for stage progress.");
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

fn queue_frame(ui: &mut Ui, margin: i8, content: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::color(ui.ctx(), SURFACE))
        .stroke(egui::Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
        .corner_radius(12)
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}

fn queue_count(ui: &mut Ui, count: usize, label: &str, color: egui::Color32) {
    let color = if count > 0 { color } else { MUTED };
    ui.label(theme::strong(format!("{count} {label}"), 13.0).color(theme::color(ui.ctx(), color)));
}

fn queue_tag(ui: &mut Ui, title: &str, tone: egui::Color32) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0_f32, theme::color(ui.ctx(), LINE)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(theme::strong(title, 12.0).color(theme::color(ui.ctx(), tone)));
        });
}

enum QueueGlyph {
    Active,
    Ready,
    Layers,
}

fn queue_glyph(ui: &mut Ui, shape: QueueGlyph, tone: egui::Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    let painter = ui.painter();
    let border = theme::color(ui.ctx(), LINE);
    if size > 30.0 {
        painter.rect(
            rect,
            10,
            theme::color(ui.ctx(), CREAM),
            egui::Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
    }
    let stroke = egui::Stroke::new(
        if size > 30.0 { 2.0_f32 } else { 1.2_f32 },
        theme::color(ui.ctx(), tone),
    );
    let at = |x, y| rect.min + egui::vec2(x, y) * size;
    let path = match shape {
        QueueGlyph::Active => vec![
            at(0.18, 0.54),
            at(0.33, 0.54),
            at(0.43, 0.31),
            at(0.54, 0.70),
            at(0.64, 0.46),
            at(0.82, 0.46),
        ],
        QueueGlyph::Ready => vec![at(0.28, 0.51), at(0.44, 0.66), at(0.72, 0.36)],
        QueueGlyph::Layers => {
            painter.add(egui::Shape::line(
                vec![at(0.18, 0.58), at(0.50, 0.75), at(0.82, 0.58)],
                stroke,
            ));
            vec![
                at(0.13, 0.45),
                at(0.50, 0.25),
                at(0.87, 0.45),
                at(0.50, 0.65),
                at(0.13, 0.45),
            ]
        }
    };
    painter.add(egui::Shape::line(path, stroke));
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
            pending_ids: Vec::new(),
        }
    }
}
