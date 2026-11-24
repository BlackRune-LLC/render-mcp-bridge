use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::models::ModelFile;
use crate::setup::{atomic_write, fetch_json, loopback_agent, validate_comfy_url};

const INVENTORY_TIMEOUT: Duration = Duration::from_secs(12);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CATEGORIES: usize = 64;
const DESTINATION_FILE: &str = "model-destination.json";
const MAX_DESTINATION_BYTES: u64 = 64 * 1024;

#[derive(Debug, Default)]
pub struct Library {
    pub files: BTreeMap<String, Vec<String>>,
    paths: Option<BTreeMap<String, Vec<PathBuf>>>,
}

pub fn inspect(url: &str) -> Result<Library, String> {
    let origin = validate_comfy_url(url)?;
    let agent = loopback_agent(REQUEST_TIMEOUT);
    let categories = fetch_json(&agent, &origin, "/models").ok_or(
        "ComfyUI did not report its model library. Start the selected build and try again.",
    )?;
    let categories = categories
        .as_array()
        .ok_or("ComfyUI returned an invalid model library.")?;
    let end = Instant::now() + INVENTORY_TIMEOUT;
    let mut files = BTreeMap::new();
    for category in categories
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| valid_category(name))
        .take(MAX_CATEGORIES)
    {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let request = loopback_agent(remaining.min(REQUEST_TIMEOUT));
        if let Some(names) = fetch_json(&request, &origin, &format!("/models/{category}"))
            .and_then(|data| model_names(&data))
        {
            files.insert(category.to_owned(), names);
        }
    }
    if files.is_empty() && !categories.is_empty() {
        return Err(
            "ComfyUI did not report any model categories. Try refreshing the library.".into(),
        );
    }
    // Older builds and some proxies do not expose this frontend route. The
    // public model APIs still work and the user can confirm a local destination.
    let paths = fetch_json(&agent, &origin, "/internal/folder_paths")
        .and_then(|data| reported_paths(&data));
    Ok(Library { files, paths })
}

impl Library {
    pub fn reports_category(&self, file: &ModelFile) -> bool {
        model_category(file).is_some_and(|category| self.files.contains_key(category))
    }

    pub fn has(&self, file: &ModelFile) -> bool {
        model_category(file)
            .and_then(|category| self.files.get(category))
            .is_some_and(|names| {
                names
                    .iter()
                    .any(|name| name.rsplit(['/', '\\']).next() == Some(file.filename))
            })
    }

    pub fn paths_reported(&self) -> bool {
        self.paths.is_some()
    }

    pub fn destination(
        &self,
        requested: Option<&Path>,
        fallback: &Path,
        files: &[ModelFile],
    ) -> Result<PathBuf, String> {
        let categories = files
            .iter()
            .map(model_category)
            .collect::<Option<BTreeSet<_>>>()
            .filter(|categories| !categories.is_empty())
            .ok_or("This download has an invalid model category.")?;
        let matches = |root: &Path| {
            self.paths.as_ref().is_none_or(|paths| {
                categories
                    .iter()
                    .all(|category| registered(root, category, paths))
            })
        };
        if let Some(requested) = requested {
            let root = existing_local_directory(requested)
                .ok_or("Choose an existing model folder on a local drive.")?;
            return if matches(&root) {
                Ok(root)
            } else {
                Err("This folder is not registered for all the selected model categories in ComfyUI.".into())
            };
        }
        if let Some(root) = existing_local_directory(fallback).filter(|root| matches(root)) {
            return Ok(root);
        }
        if let Some(paths) = &self.paths
            && let Some(first) = categories.first()
            && let Some(root) = paths
                .get(*first)
                .into_iter()
                .flatten()
                .filter_map(|path| path.parent())
                .filter_map(existing_local_directory)
                .find(|root| matches(root))
        {
            return Ok(root);
        }
        Err(
            "Choose the model folder used by this ComfyUI build for all the selected categories."
                .into(),
        )
    }
}

pub fn load_destination(data: &Path) -> Option<PathBuf> {
    let preference = existing_local_directory(data)?.join(DESTINATION_FILE);
    if !fs::symlink_metadata(&preference).ok()?.is_file() {
        return None;
    }
    let file = File::open(preference).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_DESTINATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_DESTINATION_BYTES {
        return None;
    }
    let path = serde_json::from_slice::<Option<PathBuf>>(&bytes).ok()??;
    existing_local_directory(&path)
}

pub fn save_destination(data: &Path, selected: Option<&Path>) -> Result<(), String> {
    let data = existing_local_directory(data)
        .ok_or("Use an existing Render Bridge data folder on a local drive.")?;
    let folder = selected
        .map(|path| {
            existing_local_directory(path)
                .ok_or("Choose an existing model folder on a local drive.")
        })
        .transpose()?;
    let bytes =
        serde_json::to_vec(&folder).map_err(|_| "Could not encode the model folder preference.")?;
    if bytes.len() as u64 > MAX_DESTINATION_BYTES {
        return Err("The model folder path is too long.".into());
    }
    atomic_write(&data.join(DESTINATION_FILE), &bytes)
        .map_err(|_| "Could not save the model folder preference.".into())
}

fn model_category(file: &ModelFile) -> Option<&str> {
    file.folder
        .strip_prefix("models/")?
        .strip_suffix('/')
        .filter(|value| valid_category(value))
}

fn valid_category(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !matches!(name, "configs" | "custom_nodes")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn model_names(data: &Value) -> Option<Vec<String>> {
    let mut names = data
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 1024
                && !name.chars().any(char::is_control)
                && name.split(['/', '\\']).all(|component| {
                    !matches!(component, "" | "." | "..") && !component.contains(':')
                })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    Some(names)
}

fn reported_paths(data: &Value) -> Option<BTreeMap<String, Vec<PathBuf>>> {
    Some(
        data.as_object()?
            .iter()
            .filter(|(category, _)| valid_category(category))
            .filter_map(|(category, values)| {
                let paths = values
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(PathBuf::from)
                    .filter(|path| local_absolute_path(path))
                    .collect();
                Some((category.to_owned(), paths))
            })
            .collect(),
    )
}

fn registered(root: &Path, category: &str, paths: &BTreeMap<String, Vec<PathBuf>>) -> bool {
    let Some(directory) = category_location(&root.join(category)) else {
        return false;
    };
    paths
        .get(category)
        .into_iter()
        .flatten()
        .any(|path| category_location(path).is_some_and(|path| same_location(&directory, &path)))
}

fn category_location(path: &Path) -> Option<PathBuf> {
    let parent = existing_local_directory(path.parent()?)?;
    let path = parent.join(path.file_name()?);
    match fs::symlink_metadata(&path) {
        Ok(_) => existing_local_directory(&path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(path),
        Err(_) => None,
    }
}

fn existing_local_directory(path: &Path) -> Option<PathBuf> {
    if !local_absolute_path(path) {
        return None;
    }
    #[cfg(windows)]
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        crate::asset_folders::check_directory(ancestor).ok()?;
    }
    #[cfg(not(windows))]
    crate::asset_folders::check_directory(path).ok()?;
    let path = fs::canonicalize(path).ok()?;
    local_absolute_path(&path).then_some(path)
}

fn local_absolute_path(path: &Path) -> bool {
    if !path.is_absolute()
        || !path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
    {
        return false;
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        )
    }
    #[cfg(not(windows))]
    {
        true
    }
}

fn same_location(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    use serde_json::json;

    fn pack() -> &'static [ModelFile] {
        crate::models::STARTER_PIPELINES[0].files
    }

    #[test]
    fn excludes_unusable_paths_and_respects_the_builds_reported_destination() {
        let temp = tempfile::tempdir().unwrap();
        let paired = temp.path().join("paired");
        let external = temp.path().join("external");
        fs::create_dir(&paired).unwrap();
        fs::create_dir(&external).unwrap();
        let paths = reported_paths(&json!({
            "diffusion_models": ["relative", external.join("diffusion_models")],
            "text_encoders": [external.join("text_encoders")],
            "vae": [external.join("vae")],
            "custom_nodes": [temp.path().join("custom_nodes")],
        }))
        .unwrap();
        assert_eq!(paths.len(), 3);
        assert_eq!(
            paths["diffusion_models"],
            [external.join("diffusion_models")]
        );
        let library = Library {
            paths: Some(paths),
            ..Library::default()
        };
        assert!(library.paths_reported());
        assert!(library.destination(Some(&paired), &paired, pack()).is_err());
        assert_eq!(
            library.destination(None, &paired, pack()).unwrap(),
            fs::canonicalize(&external).unwrap()
        );
        assert_eq!(
            library
                .destination(Some(&external), &paired, pack())
                .unwrap(),
            fs::canonicalize(&external).unwrap()
        );
    }

    #[test]
    fn accepts_a_paired_or_explicit_folder_on_builds_without_the_paths_route() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = temp.path().join("explicit");
        fs::create_dir(&explicit).unwrap();
        let library = Library::default();
        assert!(!library.paths_reported());
        assert_eq!(
            library.destination(None, temp.path(), pack()).unwrap(),
            fs::canonicalize(temp.path()).unwrap()
        );
        assert_eq!(
            library
                .destination(Some(&explicit), temp.path(), pack())
                .unwrap(),
            fs::canonicalize(explicit).unwrap()
        );
        assert!(
            library
                .destination(Some(Path::new("relative")), temp.path(), pack())
                .is_err()
        );
    }

    #[test]
    fn reads_inventory_from_the_local_model_routes_without_requiring_the_paths_route() {
        let filename = pack()[0].filename;
        let names = json!([
            format!("subfolder/{filename}"),
            "bad/../unknown.safetensors"
        ]);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (endpoint, status, body) in [
                (
                    "/models",
                    "200 OK",
                    r#"["diffusion_models","../unsafe"]"#.to_owned(),
                ),
                ("/models/diffusion_models", "200 OK", names.to_string()),
                ("/internal/folder_paths", "404 Not Found", "{}".to_owned()),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(REQUEST_TIMEOUT)).unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert_eq!(request, format!("GET {endpoint} HTTP/1.1\r\n"));
                loop {
                    request.clear();
                    reader.read_line(&mut request).unwrap();
                    if request == "\r\n" || request.is_empty() {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len(),
                )
                .unwrap();
            }
        });
        let library = inspect(&url).unwrap();
        worker.join().unwrap();
        assert!(!library.paths_reported());
        assert_eq!(
            library.files["diffusion_models"],
            [format!("subfolder/{filename}")]
        );
        assert!(library.has(&pack()[0]));
        assert!(inspect("https://example.com").is_err());
    }

    #[test]
    fn saves_a_per_install_destination_without_changing_setup_settings() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("profile");
        let models = temp.path().join("models");
        fs::create_dir(&data).unwrap();
        fs::create_dir(&models).unwrap();
        assert!(load_destination(&data).is_none());
        save_destination(&data, Some(&models)).unwrap();
        assert_eq!(
            load_destination(&data),
            Some(fs::canonicalize(models).unwrap())
        );
        assert!(!data.join("settings.json").exists());
        save_destination(&data, None).unwrap();
        assert!(load_destination(&data).is_none());
    }
}
