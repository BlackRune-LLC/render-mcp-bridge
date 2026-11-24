use eframe::egui::{self, Color32, Frame, RichText, Shape, Stroke, Ui};

use crate::{LINE, SURFACE, VIOLET, caption, theme};

#[derive(Clone, Copy)]
pub(crate) enum UtilityIcon {
    Download,
    Maintenance,
}

pub(crate) fn heading(ui: &mut Ui, icon: UtilityIcon, title: &str, detail: Option<&str>) {
    ui.horizontal_top(|ui| {
        let (bounds, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
        let line = theme::color(ui.ctx(), LINE);
        let fill = theme::color(ui.ctx(), SURFACE);
        let accent = theme::color(ui.ctx(), VIOLET);
        ui.painter().rect(
            bounds,
            10,
            fill,
            Stroke::new(1.0_f32, line),
            egui::StrokeKind::Inside,
        );
        draw(ui.painter(), bounds, icon, accent);
        ui.add_space(5.0);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(title).size(22.0).strong()).wrap());
            if let Some(detail) = detail {
                caption(ui, detail);
            }
        });
    });
    ui.add_space(6.0);
}

fn draw(painter: &egui::Painter, bounds: egui::Rect, icon: UtilityIcon, color: Color32) {
    let stroke = Stroke::new(2.6_f32, color);
    let at = |x, y| bounds.min + egui::vec2(x, y);
    match icon {
        UtilityIcon::Download => {
            painter.line_segment([at(24.0, 11.0), at(24.0, 28.0)], stroke);
            painter.add(Shape::line(
                vec![at(17.0, 22.0), at(24.0, 29.0), at(31.0, 22.0)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![
                    at(12.0, 30.0),
                    at(12.0, 36.0),
                    at(36.0, 36.0),
                    at(36.0, 30.0),
                ],
                stroke,
            ));
        }
        UtilityIcon::Maintenance => {
            painter.add(Shape::line(
                vec![
                    at(32.0, 12.0),
                    at(28.0, 17.0),
                    at(30.0, 21.0),
                    at(35.0, 22.0),
                    at(40.0, 17.0),
                    at(39.0, 25.0),
                    at(34.0, 29.0),
                    at(28.0, 29.0),
                    at(18.0, 39.0),
                    at(12.0, 33.0),
                    at(23.0, 23.0),
                    at(23.0, 17.0),
                    at(27.0, 13.0),
                    at(32.0, 12.0),
                ],
                stroke,
            ));
        }
    }
}

pub(crate) fn footer(ui: &mut Ui) {
    ui.add_space(5.0);
    ui.separator();
    ui.add_space(5.0);
}

pub(crate) fn scroll_body(ui: &mut Ui, id: &'static str, content: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height((ui.ctx().content_rect().height() - 184.0).max(50.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(egui::Margin::symmetric(0, 3))
                .show(ui, content);
        });
}
