use serde_json::Value;

#[derive(Clone)]
pub struct ModelFile {
    pub node: &'static str,
    pub field: &'static str,
    pub filename: &'static str,
    pub folder: &'static str,
    pub source: &'static str,
    pub download: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct Pipeline {
    pub name: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub files: &'static [ModelFile],
}

const Z_IMAGE_TURBO: Pipeline = Pipeline {
    name: "Z-Image Turbo",
    description: "Recommended first render for text-to-image. Three files, about 15 GB total.",
    license: "Upstream Apache-2.0 · SwarmUI FP8 conversion",
    files: &[
        ModelFile {
            node: "UNETLoader",
            field: "unet_name",
            filename: "SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
            folder: "models/diffusion_models/",
            source: "https://huggingface.co/mcmonkey/swarm-models/blob/441fb3dcb48f31d2b006c8a790cc10cf9714b972/SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
            download: "https://huggingface.co/mcmonkey/swarm-models/resolve/441fb3dcb48f31d2b006c8a790cc10cf9714b972/SwarmUI_Z-Image-Turbo-FP8Mix.safetensors",
            size: 6_571_226_600,
            sha256: "ba92d3705131c8d9b05ca9c6fefe39444d4eb02db16c30aafa9fcf5f85230e06",
        },
        ModelFile {
            node: "CLIPLoader",
            field: "clip_name",
            filename: "qwen_3_4b.safetensors",
            folder: "models/text_encoders/",
            source: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/429cce00bd1829a3dfd6c57c3204772542876215/split_files/text_encoders/qwen_3_4b.safetensors",
            download: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/resolve/429cce00bd1829a3dfd6c57c3204772542876215/split_files/text_encoders/qwen_3_4b.safetensors",
            size: 8_044_982_048,
            sha256: "6c671498573ac2f7a5501502ccce8d2b08ea6ca2f661c458e708f36b36edfc5a",
        },
        ModelFile {
            node: "VAELoader",
            field: "vae_name",
            filename: "ae.safetensors",
            folder: "models/vae/",
            source: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/blob/08d04455279082882deaabc8d0d09fc914c071e1/split_files/vae/ae.safetensors",
            download: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/resolve/08d04455279082882deaabc8d0d09fc914c071e1/split_files/vae/ae.safetensors",
            size: 335_304_388,
            sha256: "afc8e28272cd15db3919bacdb6918ce9c1ed22e96cb12c4d5ed0fba823529e38",
        },
    ],
};

const NETA_YUME: Pipeline = Pipeline {
    name: "NetaYume",
    description: "Optional illustration preset. One checkpoint.",
    license: "Check the terms for bundled components",
    files: &[ModelFile {
        node: "CheckpointLoaderSimple",
        field: "ckpt_name",
        filename: "NetaYumev35_pretrained_all_in_one.safetensors",
        folder: "models/checkpoints/",
        source: "https://huggingface.co/duongve/NetaYume-Lumina-Image-2.0/blob/main/NetaYumev35_pretrained_all_in_one.safetensors",
        download: "",
        size: 10_620_231_237,
        sha256: "4125cb490996ea85c8e3ba242866da02efd83a6a2c079dc8924a95eaa8327a44",
    }],
};

const BIREFNET_FILE: ModelFile = ModelFile {
    node: "LoadBackgroundRemovalModel",
    field: "bg_removal_name",
    filename: "birefnet.safetensors",
    folder: "models/background_removal/",
    source: "https://huggingface.co/Comfy-Org/BiRefNet/blob/35767b272f2846752a3aee1259abdd4586f735c8/background_removal/birefnet.safetensors",
    download: "https://huggingface.co/Comfy-Org/BiRefNet/resolve/35767b272f2846752a3aee1259abdd4586f735c8/background_removal/birefnet.safetensors",
    size: 444_473_596,
    sha256: "9ab37426bf4de0567af6b5d21b16151357149139362e6e8992021b8ce356a154",
};

const BIREFNET: Pipeline = Pipeline {
    name: "BiRefNet",
    description: "Remove backgrounds and create transparent PNGs. Requires current ComfyUI core nodes.",
    license: "MIT",
    files: &[BIREFNET_FILE],
};

const WAN_ANIMATE: Pipeline = Pipeline {
    name: "Wan Animate 2",
    description: "Transfer motion from a driving clip to a still image. About 25 GB total, including BiRefNet for transparent frames.",
    license: "Apache-2.0 · BiRefNet MIT",
    files: &[
        ModelFile {
            node: "UNETLoader",
            field: "unet_name",
            filename: "wan_animate_2_distill_int8_convrot.safetensors",
            folder: "models/diffusion_models/",
            source: "https://huggingface.co/Comfy-Org/Wan-Animate-2/blob/924563469bb8ac056e6171c3123a12903317606d/diffusion_models/wan_animate_2_distill_int8_convrot.safetensors",
            download: "https://huggingface.co/Comfy-Org/Wan-Animate-2/resolve/924563469bb8ac056e6171c3123a12903317606d/diffusion_models/wan_animate_2_distill_int8_convrot.safetensors",
            size: 16_653_175_528,
            sha256: "d2e566ecac3f164cb1a6b63dc7d868d6678c9affa49fbceb3abe4f9458f89c8a",
        },
        ModelFile {
            node: "CLIPLoader",
            field: "clip_name",
            filename: "umt5_xxl_fp8_e4m3fn_scaled.safetensors",
            folder: "models/text_encoders/",
            source: "https://huggingface.co/Comfy-Org/Wan-Animate-2/blob/924563469bb8ac056e6171c3123a12903317606d/text_encoders/umt5_xxl_fp8_e4m3fn_scaled.safetensors",
            download: "https://huggingface.co/Comfy-Org/Wan-Animate-2/resolve/924563469bb8ac056e6171c3123a12903317606d/text_encoders/umt5_xxl_fp8_e4m3fn_scaled.safetensors",
            size: 6_735_906_897,
            sha256: "c3355d30191f1f066b26d93fba017ae9809dce6c627dda5f6a66eaa651204f68",
        },
        ModelFile {
            node: "VAELoader",
            field: "vae_name",
            filename: "Wan2_1_VAE_bf16.safetensors",
            folder: "models/vae/",
            source: "https://huggingface.co/Comfy-Org/Wan-Animate-2/blob/924563469bb8ac056e6171c3123a12903317606d/vae/Wan2_1_VAE_bf16.safetensors",
            download: "https://huggingface.co/Comfy-Org/Wan-Animate-2/resolve/924563469bb8ac056e6171c3123a12903317606d/vae/Wan2_1_VAE_bf16.safetensors",
            size: 253_806_278,
            sha256: "1ab9a32cc2c740f6e39d80d367ce5dcc28db8c71b79b28670546b8973e9d75f9",
        },
        ModelFile {
            node: "CLIPVisionLoader",
            field: "clip_name",
            filename: "clip_vision_h.safetensors",
            folder: "models/clip_vision/",
            source: "https://huggingface.co/Comfy-Org/Wan-Animate-2/blob/924563469bb8ac056e6171c3123a12903317606d/clip_vision/clip_vision_h.safetensors",
            download: "https://huggingface.co/Comfy-Org/Wan-Animate-2/resolve/924563469bb8ac056e6171c3123a12903317606d/clip_vision/clip_vision_h.safetensors",
            size: 1_264_219_396,
            sha256: "64a7ef761bfccbadbaa3da77366aac4185a6c58fa5de5f589b42a65bcc21f161",
        },
        BIREFNET_FILE,
    ],
};

pub const STARTER_PIPELINES: &[Pipeline] = &[Z_IMAGE_TURBO, NETA_YUME];
pub const DOWNLOAD_PIPELINES: &[Pipeline] = &[Z_IMAGE_TURBO, BIREFNET, WAN_ANIMATE];

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
