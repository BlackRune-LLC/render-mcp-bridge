use serde_json::Value;

pub struct ModelFile {
    pub node: &'static str,
    pub field: &'static str,
    pub filename: &'static str,
    pub folder: &'static str,
    pub source: &'static str,
}

pub struct Pipeline {
    pub name: &'static str,
    pub description: &'static str,
    pub files: &'static [ModelFile],
}

pub const STARTER_PIPELINES: &[Pipeline] = &[
    Pipeline {
        name: "Z-Image Turbo",
        description: "Recommended first render. Three files, about 15 GB total.",
        files: &[
            ModelFile {
                node: "UNETLoader",
                field: "unet_name",
                filename: "SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
                folder: "models/diffusion_models/",
                source: "https://huggingface.co/mcmonkey/swarm-models/blob/main/SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
            },
            ModelFile {
                node: "CLIPLoader",
                field: "clip_name",
                filename: "qwen_3_4b.safetensors",
                folder: "models/text_encoders/",
                source: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/main/split_files/text_encoders/qwen_3_4b.safetensors",
            },
            ModelFile {
                node: "VAELoader",
                field: "vae_name",
                filename: "ae.safetensors",
                folder: "models/vae/",
                source: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/main/split_files/vae/ae.safetensors",
            },
        ],
    },
    Pipeline {
        name: "NetaYume",
        description: "Optional illustration preset. One checkpoint.",
        files: &[ModelFile {
            node: "CheckpointLoaderSimple",
            field: "ckpt_name",
            filename: "NetaYumev35_pretrained_all_in_one.safetensors",
            folder: "models/checkpoints/",
            source: "https://huggingface.co/duongve/NetaYume-Lumina-Image-2.0/blob/main/NetaYumev35_pretrained_all_in_one.safetensors",
        }],
    },
];

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hardware {
    device: Option<String>,
    vram: Option<u64>,
    ram: Option<u64>,
    shared_memory: bool,
}

impl Hardware {
    pub fn from_stats(stats: &Value) -> Option<Self> {
        let nonzero = |value: &Value| value.as_u64().filter(|bytes| *bytes > 0);
        let ram = nonzero(&stats["system"]["ram_total"]);
        // ComfyUI puts the device it actually uses first, even if another
        // installed accelerator has more memory.
        let device = &stats["devices"][0];
        let kind = device["type"].as_str().unwrap_or_default();
        let name = device["name"].as_str().unwrap_or_default().trim();
        let cpu = kind.eq_ignore_ascii_case("cpu") || name.eq_ignore_ascii_case("cpu");
        let shared_memory = kind.eq_ignore_ascii_case("mps");
        let vram = if cpu {
            None
        } else {
            nonzero(&device["vram_total"])
        };
        let device = if cpu {
            Some("CPU mode".into())
        } else if shared_memory && (name.is_empty() || name.eq_ignore_ascii_case("mps")) {
            Some("Apple GPU (MPS)".into())
        } else {
            (!name.is_empty() && !name.chars().any(char::is_control))
                .then(|| name.chars().take(160).collect())
        };
        (device.is_some() || vram.is_some() || ram.is_some()).then_some(Self {
            device,
            vram,
            ram,
            shared_memory,
        })
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(device) = &self.device {
            parts.push(device.clone());
        }
        if let Some(vram) = self.vram {
            let label = if self.shared_memory {
                "shared GPU memory"
            } else {
                "VRAM"
            };
            parts.push(format!("{:.1} GiB {label}", vram as f64 / GIB as f64));
        }
        if let Some(ram) = self.ram {
            parts.push(format!("{:.1} GiB system RAM", ram as f64 / GIB as f64));
        }
        parts.join(" · ")
    }

    pub fn recommendation(&self) -> &'static str {
        if !self.shared_memory
            && self.vram.is_some_and(|bytes| bytes >= 12 * GIB)
            && self.ram.is_some_and(|bytes| bytes >= 16 * GIB)
        {
            "Try Z-Image Turbo with one 512 × 512 image. NetaYume is the one-file starter for illustrations. Memory use varies by workflow."
        } else {
            "For Z-Image Turbo, start with one 512 × 512 image and review ComfyUI’s memory-saving settings. Check the model requirements before downloading either starter."
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileStatus {
    Found,
    Missing,
    Unknown,
}

impl From<Option<bool>> for FileStatus {
    fn from(value: Option<bool>) -> Self {
        match value {
            Some(true) => Self::Found,
            Some(false) => Self::Missing,
            None => Self::Unknown,
        }
    }
}

pub fn pipeline_status(mut files: impl Iterator<Item = FileStatus>) -> FileStatus {
    let Some(first) = files.next() else {
        return FileStatus::Unknown;
    };
    files.fold(first, |status, file| match (status, file) {
        (FileStatus::Missing, _) | (_, FileStatus::Missing) => FileStatus::Missing,
        (FileStatus::Unknown, _) | (_, FileStatus::Unknown) => FileStatus::Unknown,
        _ => FileStatus::Found,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarizes_the_device_comfyui_actually_uses() {
        let hardware = Hardware::from_stats(&json!({
            "system": {"ram_total": 31 * GIB},
            "devices": [
                {"name": "Reported GPU", "type": "cuda", "vram_total": 12 * GIB},
                {"name": "Larger inactive GPU", "type": "cuda", "vram_total": 48 * GIB}
            ]
        }))
        .unwrap();
        assert_eq!(
            hardware.summary(),
            "Reported GPU · 12.0 GiB VRAM · 31.0 GiB system RAM"
        );
        assert!(hardware.recommendation().contains("NetaYume"));
    }

    #[test]
    fn distinguishes_shared_memory_and_excludes_cpu_memory_from_vram() {
        let cpu = Hardware::from_stats(&json!({
            "system": {"ram_total": 8 * GIB},
            "devices": [{"name": "cpu", "type": "cpu", "vram_total": 8 * GIB}]
        }))
        .unwrap();
        assert_eq!(cpu.summary(), "CPU mode · 8.0 GiB system RAM");
        assert!(cpu.recommendation().contains("memory-saving"));
        let mps = Hardware::from_stats(&json!({
            "devices": [{"name": "mps", "type": "mps", "vram_total": 12 * GIB}]
        }))
        .unwrap();
        assert_eq!(
            mps.summary(),
            "Apple GPU (MPS) · 12.0 GiB shared GPU memory"
        );
        assert!(
            Hardware::from_stats(
                &json!({"system": {"ram_total": 0}, "devices": [{"vram_total": -1}]})
            )
            .is_none()
        );
    }

    #[test]
    fn reports_missing_only_for_confirmed_absence_and_ready_when_all_found() {
        use FileStatus::*;
        assert_eq!(pipeline_status([Found, Found, Found].into_iter()), Found);
        assert_eq!(
            pipeline_status([Found, Unknown, Found].into_iter()),
            Unknown
        );
        assert_eq!(
            pipeline_status([Found, Unknown, Missing].into_iter()),
            Missing
        );
        assert_eq!(pipeline_status([].into_iter()), Unknown);
    }
}
