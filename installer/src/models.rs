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
