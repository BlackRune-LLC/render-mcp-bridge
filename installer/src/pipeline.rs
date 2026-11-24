use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::setup::{atomic_write, fetch_json, lock_install, loopback_agent, validate_comfy_url};

const REVISION_FILE: &str = "pipeline-revisions.json";
const REVISION_SCHEMA: u32 = 1;
const MAX_REVISION_BYTES: u64 = 256 * 1024;
const READ_ERROR: &str = "Could not read the saved pipeline revisions.";
const INVALID_HISTORY: &str = "The saved pipeline revision file has an unsupported format.";
const SHARED_MODELS: [(&str, &str, &str); 3] = [
    (
        "CLIPLoader",
        "clip_name",
        "umt5_xxl_fp8_e4m3fn_scaled.safetensors",
    ),
    ("VAELoader", "vae_name", "Wan2_1_VAE_bf16.safetensors"),
    ("CLIPVisionLoader", "clip_name", "clip_vision_h.safetensors"),
];
const CLIP_NODES: &[&str] = &[
    "LoadImage",
    "LoadVideo",
    "PreviewAny",
    "GetVideoComponents",
    "UNETLoader",
    "CLIPLoader",
    "VAELoader",
    "CLIPVisionLoader",
    "ImageScale",
    "ImageFromBatch",
    "CLIPVisionEncode",
    "CLIPTextEncode",
    "WanAnimate2Cache",
    "ModelSamplingSD3",
    "WanAnimate2ToVideo",
    "BasicScheduler",
    "KSamplerSelect",
    "SamplerCustom",
    "TrimVideoLatent",
    "VAEDecode",
    "CreateVideo",
    "SaveVideo",
    "SaveImage",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Variant {
    #[default]
    Distilled,
    FullQuality,
}

impl Variant {
    pub fn label(self) -> &'static str {
        match self {
            Self::Distilled => "Distilled",
            Self::FullQuality => "Full quality",
        }
    }

    pub fn weight(self) -> &'static str {
        match self {
            Self::Distilled => "wan_animate_2_distill_int8_convrot.safetensors",
            Self::FullQuality => "wan_animate_2_int8_convrot.safetensors",
        }
    }

    pub fn lora(self) -> Option<&'static str> {
        match self {
            Self::Distilled => None,
            Self::FullQuality => {
                Some("lightx2v_I2V_14B_480p_cfg_step_distill_rank64_bf16.safetensors")
            }
        }
    }

    pub fn steps(self) -> u8 {
        match self {
            Self::Distilled => 10,
            Self::FullQuality => 6,
        }
    }

    pub fn agent_args(self) -> &'static str {
        match self {
            Self::Distilled => r#"{"quality":false,"transparent":false}"#,
            Self::FullQuality => r#"{"quality":true,"transparent":false}"#,
        }
    }

    fn files(self) -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
        std::iter::once(("UNETLoader", "unet_name", self.weight()))
            .chain(SHARED_MODELS)
            .chain(
                self.lora()
                    .map(|name| ("LoraLoaderModelOnly", "lora_name", name)),
            )
    }

    fn nodes(self) -> impl Iterator<Item = &'static str> {
        CLIP_NODES
            .iter()
            .copied()
            .chain(self.lora().map(|_| "LoraLoaderModelOnly"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Ready,
    Missing,
    Unknown,
}

pub struct Dependency {
    pub name: &'static str,
    pub availability: Availability,
}

pub struct Dependencies {
    pub files: Vec<Dependency>,
    pub nodes: Vec<Dependency>,
}

impl Dependencies {
    pub fn ready(&self) -> bool {
        self.files
            .iter()
            .chain(&self.nodes)
            .all(|item| item.availability == Availability::Ready)
    }
}

fn node_availability(value: Option<&Value>, name: &str) -> Availability {
    match value.and_then(Value::as_object) {
        Some(nodes) if nodes.is_empty() => Availability::Missing,
        Some(nodes)
            if nodes
                .get(name)
                .is_some_and(|node| node["input"].is_object()) =>
        {
            Availability::Ready
        }
        _ => Availability::Unknown,
    }
}

fn loader_reports(
    metadata: Option<&Value>,
    node: &str,
    field: &str,
    filename: &str,
) -> Option<bool> {
    let spec = &metadata?[node]["input"]["required"][field];
    let options = spec[0]
        .as_array()
        .or_else(|| spec[1]["options"].as_array())?;
    let mut found = false;
    for path in options {
        found |= path.as_str()?.rsplit(['/', '\\']).next() == Some(filename);
    }
    Some(found)
}

pub fn validate(url: &str, variant: Variant) -> Result<Dependencies, String> {
    let origin = validate_comfy_url(url)?;
    let mut files: Vec<_> = variant
        .files()
        .map(|(node, field, name)| {
            (
                (node, field),
                Dependency {
                    name,
                    availability: Availability::Unknown,
                },
            )
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    let nodes = variant
        .nodes()
        .map(|name| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let value = (!remaining.is_zero())
                .then(|| {
                    let agent = loopback_agent(remaining.min(Duration::from_secs(2)));
                    fetch_json(&agent, &origin, &format!("/object_info/{name}"))
                })
                .flatten();
            for ((loader, field), file) in &mut files {
                if *loader == name {
                    file.availability = match loader_reports(value.as_ref(), name, field, file.name)
                    {
                        Some(true) => Availability::Ready,
                        Some(false) => Availability::Missing,
                        None => Availability::Unknown,
                    };
                }
            }
            Dependency {
                name,
                availability: node_availability(value.as_ref(), name),
            }
        })
        .collect();
    Ok(Dependencies {
        files: files.into_iter().map(|(_, file)| file).collect(),
        nodes,
    })
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub number: u32,
    pub variant: Variant,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct History {
    schema: u32,
    revisions: Vec<Revision>,
}

pub fn load(data_dir: &Path) -> Result<Vec<Revision>, String> {
    if !data_dir.is_absolute() || !data_dir.is_dir() {
        return Err("Open a Render Bridge installation to save pipeline revisions.".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let path = data_dir.join(REVISION_FILE);
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(READ_ERROR.into()),
    };
    if !regular_file(&file, &path) {
        return Err(READ_ERROR.into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_REVISION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| READ_ERROR)?;
    if bytes.len() as u64 > MAX_REVISION_BYTES {
        return Err(INVALID_HISTORY.into());
    }
    let history: History = serde_json::from_slice(&bytes).map_err(|_| INVALID_HISTORY)?;
    if history.schema != REVISION_SCHEMA
        || history
            .revisions
            .iter()
            .enumerate()
            .any(|(index, revision)| usize::try_from(revision.number).ok() != Some(index + 1))
    {
        return Err(INVALID_HISTORY.into());
    }
    Ok(history.revisions)
}

fn regular_file(file: &File, path: &Path) -> bool {
    if !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    file.metadata().is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return false;
            }
        }
        metadata.is_file()
    })
}

pub fn append(data_dir: &Path, variant: Variant) -> Result<u32, String> {
    if !data_dir.is_absolute() || !data_dir.is_dir() {
        return Err("Open a Render Bridge installation to save pipeline revisions.".into());
    }
    let _lock = lock_install(data_dir)?;
    let mut revisions = load(data_dir)?;
    let number = u32::try_from(revisions.len() + 1).map_err(|_| INVALID_HISTORY)?;
    revisions.push(Revision { number, variant });
    let mut bytes = serde_json::to_vec_pretty(&History {
        schema: REVISION_SCHEMA,
        revisions,
    })
    .map_err(|_| "Could not encode the pipeline revision.")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_REVISION_BYTES {
        return Err("The pipeline revision file has reached its size limit.".into());
    }
    atomic_write(&data_dir.join(REVISION_FILE), &bytes)
        .map_err(|_| "Could not save the pipeline revision.")?;
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn quality_variants_match_the_wan_clip_tool() {
        let distilled = include_str!("../../comfy_mcp_server.py")
            .split("ANIMATE_MODELS:")
            .nth(1)
            .unwrap()
            .split("WAN_NEGATIVE")
            .next()
            .unwrap();
        for variant in [Variant::Distilled, Variant::FullQuality] {
            assert!(distilled.contains(variant.weight()));
            for (_, _, filename) in variant.files() {
                assert!(distilled.contains(filename));
            }
        }
        assert_eq!(Variant::Distilled.steps(), 10);
        assert_eq!(Variant::FullQuality.steps(), 6);
        assert!(Variant::Distilled.lora().is_none());
        assert!(Variant::FullQuality.lora().is_some());
        assert_eq!(
            Variant::FullQuality.agent_args(),
            r#"{"quality":true,"transparent":false}"#
        );
    }

    #[test]
    fn validates_a_backend_with_node_metadata_and_no_model_inventory() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let variant = Variant::FullQuality;
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            for name in variant.nodes() {
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("Fixture accept failed: {error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(&format!("GET /object_info/{name} ")));
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                drop(reader);
                let mut required = serde_json::Map::new();
                if let Some((_, field, file)) = variant.files().find(|(node, _, _)| *node == name) {
                    required.insert(field.into(), serde_json::json!([[file]]));
                }
                let body = serde_json::json!({(name): {"input": {"required": required}}});
                let body = body.to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body,
                ).unwrap();
            }
        });
        assert!(validate(&url, variant).unwrap().ready());
        worker.join().unwrap();
    }

    #[test]
    fn empty_comfy_node_metadata_confirms_missing_and_failed_requests_are_unknown() {
        assert_eq!(node_availability(None, "SaveVideo"), Availability::Unknown);
        assert_eq!(
            node_availability(Some(&serde_json::json!({})), "SaveVideo"),
            Availability::Missing
        );
        assert_eq!(
            node_availability(Some(&serde_json::json!({"SaveVideo": null})), "SaveVideo"),
            Availability::Unknown
        );
        assert_eq!(
            node_availability(
                Some(&serde_json::json!({"SaveVideo": {"input": {}}})),
                "SaveVideo"
            ),
            Availability::Ready
        );
    }

    #[test]
    fn reads_model_choices_from_both_loader_metadata_formats() {
        let traditional = serde_json::json!({
            "UNETLoader": {"input": {"required": {"unet_name": [["models/a.safetensors"]]}}}
        });
        let schema_v3 = serde_json::json!({
            "UNETLoader": {"input": {"required": {"unet_name": ["COMBO", {"options": ["a.safetensors"]}]}}}
        });
        for metadata in [traditional, schema_v3] {
            assert_eq!(
                loader_reports(Some(&metadata), "UNETLoader", "unet_name", "a.safetensors"),
                Some(true)
            );
            assert_eq!(
                loader_reports(Some(&metadata), "UNETLoader", "unet_name", "b.safetensors"),
                Some(false)
            );
            assert_eq!(
                loader_reports(Some(&metadata), "UNETLoader", "other", "a.safetensors"),
                None
            );
        }
    }

    #[test]
    fn revision_history_is_per_installation_and_preserves_invalid_existing_data() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path();
        assert_eq!(append(path, Variant::Distilled).unwrap(), 1);
        assert_eq!(append(path, Variant::FullQuality).unwrap(), 2);
        let history = load(path).unwrap();
        assert_eq!(history.last().unwrap().variant, Variant::FullQuality);
        assert_eq!(history.last().unwrap().number, 2);
        std::fs::create_dir(path.join("other")).unwrap();
        assert!(load(&path.join("other")).unwrap().is_empty());
        let file = path.join("pipeline-revisions.json");
        let before = b"{\"schema\":50,\"revisions\":[]}";
        std::fs::write(&file, before).unwrap();
        assert!(append(path, Variant::Distilled).is_err());
        assert_eq!(std::fs::read(file).unwrap(), before);
    }
}
