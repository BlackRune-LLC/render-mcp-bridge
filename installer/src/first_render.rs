use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, Ui};
use ring::rand::{SecureRandom, SystemRandom};

use crate::setup::{self, Installed};
use crate::{CAUTION, GREEN, caption, card, field_label, secondary, theme};

const MAX_LINK_BYTES: usize = 4096;
const PREVIEW_SIZE: usize = 96;
const MAX_RECEIPT_BYTES: usize = PREVIEW_SIZE * PREVIEW_SIZE * 8 + 4096;
const PREVIEW_MODE: &str = "RENDER_BRIDGE_FIRST_RENDER_PREVIEW";

type CheckedReceipt = Result<Option<egui::ColorImage>, &'static str>;

#[derive(Default)]
pub(crate) struct FirstRender {
    installation: Option<(PathBuf, String)>,
    prefix: Option<String>,
    link: String,
    worker: Option<Receiver<CheckedReceipt>>,
    preview: Option<egui::TextureHandle>,
    result: Option<Result<(), &'static str>>,
}

impl FirstRender {
    pub(crate) fn show(&mut self, ui: &mut Ui, installed: &Installed, server_verified: bool) {
        let installation = (installed.app_dir.clone(), installed.mcp_url.clone());
        if self.installation.as_ref() != Some(&installation) {
            *self = Self {
                installation: Some(installation),
                ..Self::default()
            };
        }
        self.poll(ui.ctx());

        card(ui, |ui| {
            theme::heading(ui, "First render", 19.0);
            if matches!(self.result, Some(Ok(()))) {
                ui.horizontal_wrapped(|ui| {
                    if let Some(preview) = &self.preview {
                        let size = egui::vec2(PREVIEW_SIZE as f32, PREVIEW_SIZE as f32);
                        ui.add(
                            egui::Image::new((preview.id(), size))
                                .corner_radius(8)
                                .alt_text("Verified first render"),
                        );
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("PNG verified")
                                .strong()
                                .color(theme::color(ui.ctx(), GREEN)),
                        );
                        caption(ui, "The signed link returned a valid 512 × 512 PNG.");
                    });
                });
                return;
            }
            if server_verified {
                caption(
                    ui,
                    "Run a 512 × 512 test with the Z-Image Turbo starter pack, then verify the signed result here.",
                );
            } else {
                caption(
                    ui,
                    "Start & Verify enables the first-render test. The test runs only when you give its prompt to a connected agent.",
                );
            }
            let copy = if self.prefix.is_some() {
                "Copy test prompt again"
            } else {
                "Copy first render test"
            };
            if secondary(ui, copy, server_verified && self.worker.is_none()).clicked() {
                if self.prefix.is_none() {
                    self.prefix = new_prefix();
                }
                if let Some(prefix) = &self.prefix {
                    ui.ctx().copy_text(test_prompt(&installed.mcp_url, prefix));
                    self.result = None;
                } else {
                    self.result = Some(Err(
                        "Could not start a new test. Try copying the prompt again.",
                    ));
                }
            }
            if self.prefix.is_some() {
                caption(
                    ui,
                    "Run the copied prompt in your connected agent. Paste its signed PNG link here to check the result.",
                );
                let label = field_label(ui, "Signed PNG link");
                ui.add_enabled(
                    self.worker.is_none(),
                    egui::TextEdit::singleline(&mut self.link)
                        .desired_width(ui.available_width())
                        .password(true)
                        .char_limit(MAX_LINK_BYTES)
                        .hint_text("Paste the link from your agent"),
                )
                .labelled_by(label);
                let check_enabled =
                    self.worker.is_none() && server_verified && !self.link.trim().is_empty();
                if secondary(ui, "Verify test PNG", check_enabled).clicked() {
                    self.start_check(ui.ctx(), installed);
                }
            }
            if self.worker.is_some() {
                caption(ui, "Checking the signed PNG…");
            } else if let Some(Err(error)) = self.result {
                ui.label(egui::RichText::new(error).color(theme::color(ui.ctx(), CAUTION)));
            }
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else { return };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("The PNG check stopped. Paste the signed link again.")
            }
        };
        self.worker = None;
        self.result = Some(result.map(|preview| {
            self.preview = preview.map(|image| {
                ctx.load_texture("first-render-preview", image, egui::TextureOptions::LINEAR)
            });
        }));
    }

    fn start_check(&mut self, ctx: &egui::Context, installed: &Installed) {
        let Some(prefix) = self.prefix.clone() else {
            return;
        };
        let link = std::mem::take(&mut self.link);
        if link.len() > MAX_LINK_BYTES {
            self.result = Some(Err("Paste the signed PNG URL without any additional text."));
            return;
        }
        let ctx = ctx.clone();
        let installed = installed.clone();
        let (sender, receiver) = mpsc::channel();
        match thread::Builder::new()
            .name("verify-first-render".into())
            .spawn(move || {
                let _ = sender.send(check_link(&installed, &prefix, link.trim()));
                ctx.request_repaint();
            }) {
            Ok(_) => {
                self.result = None;
                self.worker = Some(receiver);
            }
            Err(_) => {
                self.result = Some(Err(
                    "Could not start the PNG check. Paste the signed link again.",
                ))
            }
        }
    }
}

fn new_prefix() -> Option<String> {
    let mut random = [0_u8; 12];
    SystemRandom::new().fill(&mut random).ok()?;
    let id = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some(format!("rb-first-render-{id}"))
}

fn test_prompt(mcp_url: &str, prefix: &str) -> String {
    let origin = mcp_url.strip_suffix("/mcp").unwrap_or(mcp_url);
    format!(
        "Run my first Render Bridge test using the configured MCP server at {mcp_url}. \
         Confirm the server exposes server_status, list_models, and generate_image. \
         Call server_status and check comfyui_running is true and gaming_mode is false. \
         Call list_models and check presets.z_image_turbo.available is true. \
         If any check fails, tell me what to fix before submitting a render.\n\n\
         Use generate_image for one simple red apple on a plain gray backdrop. Set preset to \
         z_image_turbo, width and height to 512, batch_size to 1, transparent to false, preview \
         to false, use_project_style to false, and filename_prefix to {prefix}. \
         Retrieve the returned signed PNG and give me its signed link on its own line so I can \
         paste it into Render Bridge. Use the download base at {origin}; if needed, combine the \
         returned signed path and query with that base while keeping the path and query unchanged. \
         Keep the MCP bearer token private."
    )
}

fn check_link(installed: &Installed, prefix: &str, link: &str) -> CheckedReceipt {
    let failed = "The PNG check could not finish. Reopen setup or paste the signed link again.";
    let mut child = setup::quiet_command(setup::venv_python(&installed.app_dir))
        .arg(installed.app_dir.join("verify_render_bridge.py"))
        .arg(&installed.mcp_url)
        .arg(&installed.token_path)
        .args(["--first-render", prefix])
        .env(PREVIEW_MODE, "1")
        .current_dir(&installed.app_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| failed)?;
    // The signed link is a private one-file credential. Send it over stdin, keeping
    // it out of process arguments, logs, and saved settings.
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(link.as_bytes()).is_ok());
    let result = child.wait_with_output().map_err(|_| failed)?;
    if !wrote || !result.status.success() || result.stdout.len() > MAX_RECEIPT_BYTES {
        return Err(failed);
    }
    let answer: serde_json::Value = serde_json::from_slice(&result.stdout).map_err(|_| failed)?;
    match answer["first_render"].as_str() {
        Some("ok") => Ok(receipt_preview(answer["preview_rgba"].as_str())),
        Some("wrong_link") => Err("Use the matching, unexpired PNG link from this setup test."),
        Some("download_failed") => Err(
            "Could not download the PNG. Check the server and paste a fresh link if it expired.",
        ),
        Some("invalid_png") => Err("The signed link did not return a complete 512 × 512 PNG."),
        Some("token_unavailable") => {
            Err("The private Bridge token could not be read. Start and verify the server again.")
        }
        _ => Err(failed),
    }
}

fn receipt_preview(preview: Option<&str>) -> Option<egui::ColorImage> {
    let preview = preview?.as_bytes();
    if preview.len() != PREVIEW_SIZE * PREVIEW_SIZE * 8 {
        return None;
    }
    let pixels = preview
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [PREVIEW_SIZE, PREVIEW_SIZE],
        &pixels,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_preview_accepts_only_exact_bounded_pixel_data() {
        let expected = "123456ff".repeat(PREVIEW_SIZE * PREVIEW_SIZE);
        let preview = receipt_preview(Some(&expected)).unwrap();
        assert_eq!(preview.size, [PREVIEW_SIZE, PREVIEW_SIZE]);
        assert_eq!(preview.pixels[0], egui::Color32::from_rgb(18, 52, 86));
        assert!(receipt_preview(None).is_none());
        assert!(receipt_preview(Some(&expected[2..])).is_none());
        assert!(receipt_preview(Some(&expected.replacen("12", "yz", 1))).is_none());
    }

    #[test]
    fn prompt_checks_readiness_and_requests_one_private_test() {
        let prefix = new_prefix().expect("system random is available");
        let prompt = test_prompt("http://127.0.0.1:8189/mcp", &prefix);
        assert_eq!(prefix.len(), "rb-first-render-".len() + 24);
        assert!(prompt.contains("http://127.0.0.1:8189/mcp"));
        assert!(prompt.contains("comfyui_running is true and gaming_mode is false"));
        assert!(prompt.contains("presets.z_image_turbo.available is true"));
        assert!(prompt.contains("batch_size to 1"));
        assert!(prompt.contains(&format!("filename_prefix to {prefix}")));
        assert!(prompt.contains("Retrieve the returned signed PNG"));
    }
}
