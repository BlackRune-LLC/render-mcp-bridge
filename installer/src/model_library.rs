use std::collections::BTreeMap;
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
        file: &ModelFile,
    ) -> Result<PathBuf, String> {
        let category =
            model_category(file).ok_or("This download has an invalid model category.")?;
        let Some(paths) = self.category_paths(category) else {
            return category_location(&requested.unwrap_or(fallback).join(category))
                .ok_or_else(|| "Choose the model folder used by this ComfyUI build.".into());
        };
        let registered: Vec<_> = paths
            .iter()
            .filter_map(|path| category_location(path))
            .collect();
        [requested, Some(fallback)]
            .into_iter()
            .flatten()
            .filter_map(|root| category_location(&root.join(category)))
            .find(|directory| registered.iter().any(|path| same_location(directory, path)))
            .or_else(|| registered.into_iter().next())
            .ok_or_else(|| format!("ComfyUI has no usable local folder registered for {category}."))
    }

    pub fn installed_path(
        &self,
        requested: Option<&Path>,
        fallback: &Path,
        file: &ModelFile,
    ) -> Result<Option<PathBuf>, String> {
        let category = model_category(file).ok_or("This model has an invalid category.")?;
        let names: Vec<_> = self
            .files
            .get(category)
            .into_iter()
            .flatten()
            .filter(|name| name.rsplit(['/', '\\']).next() == Some(file.filename))
            .collect();
        if names.is_empty() {
            return Ok(None);
        }

        let chosen;
        let directories = match self.category_paths(category) {
            Some(paths) => paths,
            None => {
                chosen = [self.destination(requested, fallback, file)?];
                &chosen
            }
        };
        let unavailable = || {
            format!(
                "Could not safely locate {} in the active library. Refresh the list and check its local folders.",
                file.filename
            )
        };
        let mut found = None;
        for directory in directories {
            let directory = match checked_local_directory(directory) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(unavailable()),
            };
            for name in &names {
                let Some(path) = local_model_file(&directory, name).map_err(|_| unavailable())?
                else {
                    continue;
                };
                if found
                    .as_deref()
                    .is_some_and(|previous| !same_location(previous, &path))
                {
                    return Err(format!(
                        "Multiple installed files match {}. Review them in ComfyUI before repairing.",
                        file.filename
                    ));
                }
                found = Some(path);
            }
        }
        found.map(Some).ok_or_else(unavailable)
    }

    fn category_paths(&self, category: &str) -> Option<&[PathBuf]> {
        self.paths
            .as_ref()
            .map(|paths| paths.get(category).map_or(&[][..], Vec::as_slice))
    }
}

pub fn load_destination(data: &Path) -> Option<PathBuf> {
    let preference = existing_local_directory(data)?.join(DESTINATION_FILE);
    if !regular_file(&fs::symlink_metadata(&preference).ok()?) {
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
        .filter(|name| safe_model_name(name))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    Some(names)
}

fn safe_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.chars().any(char::is_control)
        && name
            .split(['/', '\\'])
            .all(|component| !matches!(component, "" | "." | "..") && !component.contains(':'))
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
    checked_local_directory(path).ok()
}

fn checked_local_directory(path: &Path) -> io::Result<PathBuf> {
    if !local_absolute_path(path) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        crate::asset_folders::check_directory(ancestor)?;
    }
    let path = fs::canonicalize(path)?;
    local_absolute_path(&path)
        .then_some(path)
        .ok_or_else(|| io::ErrorKind::InvalidInput.into())
}

fn local_model_file(directory: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    if !safe_model_name(name) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut path = directory.to_path_buf();
    let mut components = name.split(['/', '\\']).peekable();
    while let Some(component) = components.next() {
        path.push(component);
        let checked = if components.peek().is_some() {
            crate::asset_folders::check_directory(&path)
        } else {
            fs::symlink_metadata(&path).and_then(|metadata| {
                if regular_file(&metadata) {
                    Ok(())
                } else {
                    Err(io::ErrorKind::InvalidInput.into())
                }
            })
        };
        match checked {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(path))
}

fn regular_file(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return false;
        }
    }
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn local_absolute_path(path: &Path) -> bool {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        || !path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
    {
        return false;
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        use windows::core::PCWSTR;
        let Some(Component::Prefix(prefix)) = path.components().next() else {
            return false;
        };
        let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
            return false;
        };
        let volume = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
        // Accept removable, fixed, optical, and RAM disk volumes. Network shares,
        // including shares mapped to a drive letter, are outside this lookup.
        matches!(
            unsafe { GetDriveTypeW(PCWSTR(volume.as_ptr())) },
            2 | 3 | 5 | 6
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
    fn uses_each_registered_category_directory_even_in_split_libraries() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let paired = root.join("models");
        let custom = root.join("custom-encoder-storage");
        fs::create_dir(&paired).unwrap();
        fs::create_dir(&custom).unwrap();
        let library = Library {
            paths: reported_paths(&json!({
                "diffusion_models": [paired.join("diffusion_models")],
                "text_encoders": [&custom],
            })),
            ..Library::default()
        };
        assert_eq!(
            library
                .destination(Some(&paired), &paired, &pack()[0])
                .unwrap(),
            paired.join("diffusion_models")
        );
        assert_eq!(
            library
                .destination(Some(&paired), &paired, &pack()[1])
                .unwrap(),
            custom
        );
    }

    #[test]
    fn excludes_unusable_paths_and_respects_the_builds_reported_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let paired = root.join("paired");
        let external = root.join("external");
        fs::create_dir(&paired).unwrap();
        fs::create_dir(&external).unwrap();
        let paths = reported_paths(&json!({
            "diffusion_models": ["relative", external.join("diffusion_models")],
            "text_encoders": [external.join("text_encoders"), paired.join("text_encoders")],
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
        assert_eq!(
            library
                .destination(Some(&paired), &paired, &pack()[0])
                .unwrap(),
            external.join("diffusion_models")
        );
        assert_eq!(
            library.destination(None, &paired, &pack()[1]).unwrap(),
            paired.join("text_encoders")
        );
        assert_eq!(
            library
                .destination(Some(&external), &paired, &pack()[1])
                .unwrap(),
            external.join("text_encoders")
        );
    }

    #[test]
    fn accepts_a_paired_or_explicit_folder_on_builds_without_the_paths_route() {
        let temp = tempfile::tempdir().unwrap();
        let paired = fs::canonicalize(temp.path()).unwrap();
        let explicit = paired.join("explicit");
        fs::create_dir(&explicit).unwrap();
        let library = Library::default();
        assert!(!library.paths_reported());
        assert_eq!(
            library.destination(None, &paired, &pack()[0]).unwrap(),
            paired.join("diffusion_models")
        );
        assert_eq!(
            library
                .destination(Some(&explicit), &paired, &pack()[0])
                .unwrap(),
            explicit.join("diffusion_models")
        );
        assert!(
            library
                .destination(Some(Path::new("relative")), &paired, &pack()[0])
                .is_err()
        );
    }

    #[test]
    fn resolves_an_advertised_subfolder_model_and_rejects_ambiguous_matches() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        let file = &pack()[0];
        fs::create_dir_all(first.join("subfolder")).unwrap();
        fs::create_dir_all(second.join("subfolder")).unwrap();
        let first_model = first.join("subfolder").join(file.filename);
        fs::write(&first_model, b"first").unwrap();
        let library = Library {
            paths: reported_paths(&json!({"diffusion_models": [&first, &second]})),
            files: BTreeMap::from([(
                "diffusion_models".into(),
                vec![format!("subfolder\\{}", file.filename)],
            )]),
        };
        assert_eq!(
            library.installed_path(None, &root, file).unwrap(),
            Some(first_model)
        );
        assert_eq!(
            library.installed_path(None, &root, &pack()[1]).unwrap(),
            None
        );
        fs::write(second.join("subfolder").join(file.filename), b"second").unwrap();
        assert!(
            library
                .installed_path(Some(&first), &root, file)
                .unwrap_err()
                .contains("Multiple")
        );
    }

    #[test]
    fn resolves_only_the_selected_path_when_category_paths_are_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let file = &pack()[0];
        let selected = root.join("selected");
        let category = selected.join("diffusion_models");
        fs::create_dir_all(&category).unwrap();
        let library = Library {
            files: BTreeMap::from([("diffusion_models".into(), vec![file.filename.into()])]),
            ..Library::default()
        };
        assert!(
            library
                .installed_path(Some(&selected), &root, file)
                .is_err()
        );
        let path = category.join(file.filename);
        fs::write(&path, b"expected file").unwrap();
        assert_eq!(
            library
                .installed_path(Some(&selected), &root, file)
                .unwrap(),
            Some(path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_an_installed_model_below_a_linked_subdirectory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let outside = root.join("outside");
        let category = root.join("model-storage");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&category).unwrap();
        let file = &pack()[0];
        fs::write(outside.join(file.filename), b"outside").unwrap();
        symlink(&outside, category.join("linked")).unwrap();
        let library = Library {
            paths: reported_paths(&json!({"diffusion_models": [&category]})),
            files: BTreeMap::from([(
                "diffusion_models".into(),
                vec![format!("linked/{}", file.filename)],
            )]),
        };
        assert!(library.installed_path(None, &root, file).is_err());
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
        let root = fs::canonicalize(temp.path()).unwrap();
        let data = root.join("profile");
        let models = root.join("models");
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
