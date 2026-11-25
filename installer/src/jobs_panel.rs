use eframe::egui::{self, Ui};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::INK;
use crate::jobs::{self, CancelOutcome, QueueStatus};
use crate::theme;
use crate::{
    CAUTION, CREAM, ERROR, GREEN, LINE, MUTED, SURFACE, VIOLET, caption, monitor_button, status,
};

#[cfg(windows)]
use crate::RAIL_ACTIVE;
#[cfg(windows)]
use crate::job_progress::{Observer, Progress};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const FRESH_FOR: Duration = Duration::from_secs(3);
const STOP_BUTTON_SIZE: f32 = 24.0;
const CANCEL_RETRY_DELAY: Duration = Duration::from_secs(5);
const CHECK_ERROR: &str = "The queue check could not start. Try Refresh.";

type Snapshot = Arc<Result<QueueStatus, String>>;

enum QueueRequest {
    Refresh,
    Cancel(String),
}

struct Cancellation {
    id: String,
    state: CancelState,
}

enum CancelState {
    Sending,
    Requested { retry_at: Instant },
    NoLongerRunning,
    Failed(String),
}

#[derive(Default)]
struct QueueState {
    snapshot: Option<Snapshot>,
    checked_at: Option<Instant>,
    checking: bool,
    manual: bool,
    stop_waiting: bool,
    cancellation: Option<Cancellation>,
}

impl QueueState {
    fn can_cancel(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_none_or(|cancel| match cancel.state {
                CancelState::Sending => false,
                CancelState::Requested { retry_at } => Instant::now() >= retry_at,
                CancelState::NoLongerRunning | CancelState::Failed(_) => true,
            })
    }

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
        let cancel_finished = if let Some(cancel) = self.cancellation.as_mut()
            && matches!(cancel.state, CancelState::Requested { .. })
            && matches!(self.snapshot.as_deref(), Some(Ok(queue))
                if queue.is_running(&cancel.id) == Some(false))
        {
            cancel.state = CancelState::NoLongerRunning;
            true
        } else {
            false
        };
        // Wake an open stop dialog if it observed an in-flight check.
        let stop_changed = !self.manual && std::mem::take(&mut self.stop_waiting);
        changed || explicit || stop_changed || cancel_finished
    }
}

pub struct QueueView {
    url: String,
    state: Arc<Mutex<QueueState>>,
    refresh: Option<Sender<QueueRequest>>,
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
                        Ok(QueueRequest::Refresh) => true,
                        Ok(QueueRequest::Cancel(id)) => {
                            let result = jobs::cancel_running(&url, &id);
                            if let Ok(mut state) = state.lock()
                                && let Some(cancel) = state.cancellation.as_mut()
                                && cancel.id == id
                            {
                                cancel.state = match result {
                                    Ok(CancelOutcome::Requested) => CancelState::Requested {
                                        retry_at: Instant::now() + CANCEL_RETRY_DELAY,
                                    },
                                    Ok(CancelOutcome::NoLongerRunning) => {
                                        CancelState::NoLongerRunning
                                    }
                                    Err(message) => CancelState::Failed(message),
                                };
                            }
                            ctx.request_repaint();
                            false
                        }
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
            if refresh.send(QueueRequest::Refresh).is_ok() {
                state.manual = true;
                return;
            }
        }
        if let Ok(mut state) = self.state.lock() {
            state.manual = true;
        }
        self.start_worker(ctx, true);
    }

    fn request_cancel(&self, id: &str, ctx: &egui::Context) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.can_cancel() {
            return;
        }
        let sent = self
            .refresh
            .as_ref()
            .is_some_and(|worker| worker.send(QueueRequest::Cancel(id.to_owned())).is_ok());
        state.cancellation = Some(Cancellation {
            id: id.to_owned(),
            state: if sent {
                CancelState::Sending
            } else {
                CancelState::Failed(
                    "Could not start the stop request. Refresh and try again.".into(),
                )
            },
        });
        ctx.request_repaint();
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
                self.show_cancel_status(ui);
            }
            None => queue_frame(ui, 12, |ui| caption(ui, "Checking the ComfyUI queue…")),
        }
        if self.state.lock().is_ok_and(|state| state.manual) {
            caption(ui, "Refreshing queue…");
        }
    }

    fn show_cancel_status(&self, ui: &mut Ui) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        let Some(cancel) = &state.cancellation else {
            return;
        };
        let job = short_job_id(&cancel.id).unwrap_or(&cancel.id);
        let (message, color) = match &cancel.state {
            CancelState::Sending => (format!("Sending stop request for job {job}…"), MUTED),
            CancelState::Requested { retry_at } => {
                let remaining = retry_at.saturating_duration_since(Instant::now());
                if remaining.is_zero()
                    && matches!(state.snapshot.as_deref(), Some(Ok(queue))
                    if queue.is_running(&cancel.id) == Some(true))
                {
                    (
                        format!("Job {job} is still running. You can retry Stop."),
                        CAUTION,
                    )
                } else {
                    if !remaining.is_zero() {
                        ui.ctx().request_repaint_after(remaining);
                    }
                    (format!("Stop requested for job {job}."), CAUTION)
                }
            }
            CancelState::NoLongerRunning => (format!("Job {job} is no longer running."), GREEN),
            CancelState::Failed(message) => (format!("Job {job} · {message}"), ERROR),
        };
        ui.label(theme::strong(message, 12.0).color(theme::color(ui.ctx(), color)));
    }

    fn queue_snapshot(&mut self, ui: &mut Ui, queue: &QueueStatus, dashboard: bool) {
        let (title, tone, shape) = if queue.running > 0 {
            ("Render in progress", GREEN, QueueGlyph::Active)
        } else if queue.queued > 0 {
            ("Waiting to render", VIOLET, QueueGlyph::Layers)
        } else {
            ("Queue idle", GREEN, QueueGlyph::Ready)
        };
        queue_frame(ui, if dashboard { 8 } else { 12 }, |ui| {
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
                    let active_job = if queue.running > 0 {
                        self.show_progress(ui, queue, dashboard)
                    } else {
                        None
                    };
                    ui.horizontal_wrapped(|ui| {
                        queue_count(ui, queue.running, "running", tone);
                        queue_count(ui, queue.queued, "queued", VIOLET);
                        if dashboard
                            && let Some(id) = active_job
                            && let Some(short) = short_job_id(id)
                        {
                            ui.label(theme::strong(format!("Job {short}"), 13.0))
                                .on_hover_text(id);
                        }
                    });
                });
            });
        });
        if dashboard {
            self.show_cancel_status(ui);
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

    fn show_progress<'a>(
        &self,
        ui: &mut Ui,
        queue: &'a QueueStatus,
        dashboard: bool,
    ) -> Option<&'a str> {
        #[cfg(windows)]
        let sample = self.progress.as_ref().and_then(|observer| {
            queue.running_ids.iter().find_map(|id| {
                observer
                    .latest_for(id)
                    .map(|progress| (id.as_str(), progress))
            })
        });
        #[cfg(windows)]
        let measured_job = sample.map(|(id, _)| id);
        #[cfg(not(windows))]
        let measured_job = None;
        let active = measured_job.or_else(|| identified_running_job(queue));
        let stoppable = active.filter(|id| dashboard && jobs::valid_prompt_id(id));
        let stop = |ui: &mut Ui| {
            if let Some(id) = stoppable {
                let enabled = self.state.lock().is_ok_and(|state| state.can_cancel());
                if stop_job_button(ui, id, enabled).clicked() {
                    self.request_cancel(id, ui.ctx());
                }
            }
        };
        #[cfg(windows)]
        if let Some((_, progress)) = sample {
            show_node_progress(ui, progress, stoppable.is_some(), stop);
            return active;
        }
        if stoppable.is_some() {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), STOP_BUTTON_SIZE),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    stop(ui);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        caption(ui, "Waiting for node progress.");
                    });
                },
            );
        } else {
            caption(ui, "Waiting for node progress.");
        }
        active
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

#[cfg(windows)]
fn show_node_progress(
    ui: &mut Ui,
    progress: Progress,
    stoppable: bool,
    stop: impl FnOnce(&mut Ui),
) {
    let fraction = (progress.value / progress.max).clamp(0.0, 1.0);
    let percent = if progress.value < progress.max {
        (fraction * 100.0).round().min(99.0)
    } else {
        100.0
    };
    ui.scope(|ui| {
        let rail_height = 10.0;
        if stoppable {
            ui.spacing_mut().item_spacing.y = 1.0;
        }
        ui.horizontal_wrapped(|ui| {
            caption(
                ui,
                &format!("Node steps {:.0} of {:.0}", progress.value, progress.max),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    theme::strong(format!("{percent:.0}% of node"), 17.0)
                        .color(theme::color(ui.ctx(), INK)),
                );
            });
        });
        let height = if stoppable {
            STOP_BUTTON_SIZE
        } else {
            rail_height
        };
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                stop(ui);
                ui.scope(|ui| {
                    ui.visuals_mut().extreme_bg_color = theme::color(ui.ctx(), RAIL_ACTIVE);
                    ui.add(
                        egui::ProgressBar::new(fraction as f32)
                            .desired_height(rail_height)
                            .fill(theme::color(ui.ctx(), GREEN)),
                    );
                });
            },
        );
    });
}

fn stop_job_button(ui: &mut Ui, id: &str, enabled: bool) -> egui::Response {
    ui.push_id(("stop-job", id), |ui| {
        ui.spacing_mut().button_padding.x = 0.0;
        let fill = theme::destructive_fill(ui.ctx());
        let response = ui.add_enabled(
            enabled,
            egui::Button::new("")
                .small()
                .fill(fill)
                .min_size(egui::Vec2::splat(STOP_BUTTON_SIZE))
                .corner_radius(6),
        );
        let glyph = egui::Rect::from_center_size(response.rect.center(), egui::Vec2::splat(9.0));
        let label = format!("Stop job {}", short_job_id(id).unwrap_or(id));
        let painter = ui.painter_at(response.rect);
        let glyph_color = theme::on_fill(fill);
        let glyph_color = if enabled {
            glyph_color
        } else {
            glyph_color.gamma_multiply(ui.visuals().disabled_alpha())
        };
        painter.rect_filled(glyph, 1, glyph_color);
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &label));
        response
            .on_hover_text(label)
            .on_disabled_hover_text("A stop request is already in progress.")
    })
    .inner
}

fn identified_running_job(queue: &QueueStatus) -> Option<&str> {
    match queue.running_ids.as_slice() {
        [id] if queue.running == 1 && jobs::valid_prompt_id(id) => Some(id),
        _ => None,
    }
}

fn short_job_id(id: &str) -> Option<&str> {
    jobs::valid_prompt_id(id).then(|| &id[..id.len().min(10)])
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

    #[cfg(windows)]
    #[test]
    fn completed_node_still_labels_the_percentage_as_node_progress() {
        let ctx = egui::Context::default();
        theme::configure_for(&ctx, egui::ThemePreference::Dark, None);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show_node_progress(
                    ui,
                    Progress {
                        value: 20.0,
                        max: 20.0,
                    },
                    false,
                    |_| {},
                );
            });
        });
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "100% of node")
        }));
    }

    #[cfg(windows)]
    #[test]
    fn progress_stop_button_has_a_label_and_fits_beside_the_rail_in_both_themes() {
        for preference in [egui::ThemePreference::Dark, egui::ThemePreference::Light] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            theme::configure_for(&ctx, preference, None);
            let mut button = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(408.0, 120.0),
                    )),
                    ..egui::RawInput::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_node_progress(
                            ui,
                            Progress {
                                value: 14.0,
                                max: 20.0,
                            },
                            true,
                            |ui| {
                                button = Some(stop_job_button(ui, "render-a", true).rect);
                            },
                        );
                    });
                },
            );

            let button = button.unwrap();
            assert_eq!(button.size(), egui::Vec2::splat(STOP_BUTTON_SIZE));
            assert!(button.bottom() < 65.0);
            let rail = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if rect.fill == theme::color(&ctx, RAIL_ACTIVE)
                            && rect.rect.height() == 10.0 =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .unwrap();
            assert!(rail.right() < button.left());
            assert!((rail.center().y - button.center().y).abs() < 1.0);
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Stop job render-a"))
            );
        }
    }

    #[test]
    fn active_job_identity_only_uses_bounded_ascii_ids() {
        assert_eq!(short_job_id("abc123-def456"), Some("abc123-def"));
        assert_eq!(short_job_id("job01"), Some("job01"));
        assert_eq!(short_job_id("prompt text"), None);
        assert_eq!(short_job_id("éx"), None);
        assert_eq!(short_job_id(&"a".repeat(129)), None);
        assert_eq!(short_job_id(""), None);
    }

    #[test]
    fn one_identifiable_running_job_can_be_stopped_before_the_first_progress_event() {
        let mut snapshot = queue(1);
        snapshot.running = 1;
        assert_eq!(identified_running_job(&snapshot), None);
        snapshot.running_ids.push("my-render".into());
        assert_eq!(identified_running_job(&snapshot), Some("my-render"));
        snapshot.running = 2;
        assert_eq!(identified_running_job(&snapshot), None);
    }

    #[test]
    fn a_running_job_can_be_retried_manually_only_after_the_cooldown() {
        let (sender, receiver) = mpsc::channel();
        let mut view = QueueView::new("http://127.0.0.1:8188".into());
        view.refresh = Some(sender);
        let ctx = egui::Context::default();
        view.request_cancel("first", &ctx);
        view.request_cancel("first", &ctx);
        view.request_cancel("second", &ctx);
        assert!(matches!(receiver.try_recv(), Ok(QueueRequest::Cancel(id)) if id == "first"));
        assert!(receiver.try_recv().is_err());

        let requested = |retry_at| CancelState::Requested { retry_at };
        view.state
            .lock()
            .unwrap()
            .cancellation
            .as_mut()
            .unwrap()
            .state = requested(Instant::now() + CANCEL_RETRY_DELAY);
        view.request_cancel("first", &ctx);
        assert!(receiver.try_recv().is_err());

        let mut active = queue(0);
        active.running = 1;
        active.running_ids.push("first".into());
        {
            let mut state = view.state.lock().unwrap();
            state.cancellation.as_mut().unwrap().state = requested(Instant::now());
            assert!(state.finish(Ok(active), false));
            assert!(state.can_cancel());
        }
        assert!(receiver.try_recv().is_err());
        view.request_cancel("first", &ctx);
        assert!(matches!(receiver.try_recv(), Ok(QueueRequest::Cancel(id)) if id == "first"));

        let mut state = view.state.lock().unwrap();
        state.cancellation.as_mut().unwrap().state = requested(Instant::now() + CANCEL_RETRY_DELAY);
        assert!(state.finish(Ok(queue(0)), false));
        assert!(matches!(
            state.cancellation.as_ref().unwrap().state,
            CancelState::NoLongerRunning
        ));
        assert!(state.can_cancel());
    }

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
