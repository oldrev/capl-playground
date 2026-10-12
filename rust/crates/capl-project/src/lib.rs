use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
};

pub const PROJECT_EXTENSION: &str = "caplproj";
pub const PROJECT_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectManifest {
    pub format_version: u32,
    pub name: String,
    pub source_files: Vec<String>,
    pub traces: Vec<String>,
    pub databases: Vec<String>,
    pub active_source: Option<String>,
}

impl ProjectManifest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format_version: PROJECT_FORMAT_VERSION,
            name: name.into(),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Unsupported(String),
}
impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Json(e) => e.fmt(f),
            Self::Unsupported(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for ProjectError {}
impl From<std::io::Error> for ProjectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for ProjectError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

pub trait ProjectStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ProjectError>;
    fn save(&mut self, key: &str, value: &[u8]) -> Result<(), ProjectError>;
}

pub fn encode(manifest: &ProjectManifest) -> Result<Vec<u8>, ProjectError> {
    validate_version(manifest.format_version)?;
    Ok(serde_json::to_vec_pretty(manifest)?)
}
pub fn decode(bytes: &[u8]) -> Result<ProjectManifest, ProjectError> {
    let manifest: ProjectManifest = serde_json::from_slice(bytes)?;
    validate_version(manifest.format_version)?;
    Ok(manifest)
}

fn validate_version(version: u32) -> Result<(), ProjectError> {
    if version != PROJECT_FORMAT_VERSION {
        return Err(ProjectError::Unsupported(format!(
            "unsupported project format version {version}; expected {PROJECT_FORMAT_VERSION}"
        )));
    }
    Ok(())
}

pub struct FsProjectStore {
    root: PathBuf,
}
impl FsProjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, key: &str) -> Result<PathBuf, ProjectError> {
        let key_path = Path::new(key);
        if key_path.is_absolute()
            || key_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ProjectError::Unsupported(
                "project key must stay below the project store root".into(),
            ));
        }
        let path = self.root.join(key_path);
        if path.extension().and_then(|e| e.to_str()) != Some(PROJECT_EXTENSION) {
            return Err(ProjectError::Unsupported(format!(
                "project file must use .{PROJECT_EXTENSION}"
            )));
        }
        Ok(path)
    }
}
impl ProjectStore for FsProjectStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ProjectError> {
        let path = self.path(key)?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&mut self, key: &str, value: &[u8]) -> Result<(), ProjectError> {
        let path = self.path(key)?;
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(path, value)?;
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryProjectStore {
    entries: std::collections::BTreeMap<String, Vec<u8>>,
}

/// Browser adapter for a future GPUI/WASM shell. Keeping it here means the
/// project format and storage contract stay identical between native and web.
#[cfg(all(target_arch = "wasm32", feature = "web-storage"))]
pub struct LocalStorageProjectStore {
    storage: web_sys::Storage,
    namespace: String,
}

#[cfg(all(target_arch = "wasm32", feature = "web-storage"))]
impl LocalStorageProjectStore {
    pub fn new(namespace: impl Into<String>) -> Result<Self, ProjectError> {
        let window = web_sys::window()
            .ok_or_else(|| ProjectError::Unsupported("browser window unavailable".into()))?;
        let storage = window
            .local_storage()
            .map_err(|e| ProjectError::Unsupported(format!("localStorage unavailable: {e:?}")))?
            .ok_or_else(|| ProjectError::Unsupported("localStorage unavailable".into()))?;
        Ok(Self {
            storage,
            namespace: namespace.into(),
        })
    }
    fn key(&self, key: &str) -> String {
        format!("{}:{key}", self.namespace)
    }
}

#[cfg(all(target_arch = "wasm32", feature = "web-storage"))]
impl ProjectStore for LocalStorageProjectStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ProjectError> {
        Ok(self
            .storage
            .get_item(&self.key(key))
            .map_err(|e| ProjectError::Unsupported(format!("localStorage read failed: {e:?}")))?
            .map(|value| value.into_bytes()))
    }
    fn save(&mut self, key: &str, value: &[u8]) -> Result<(), ProjectError> {
        let value = String::from_utf8(value.to_vec())
            .map_err(|_| ProjectError::Unsupported("project data must be UTF-8 JSON".into()))?;
        self.storage
            .set_item(&self.key(key), &value)
            .map_err(|e| ProjectError::Unsupported(format!("localStorage write failed: {e:?}")))?;
        Ok(())
    }
}
impl ProjectStore for MemoryProjectStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, ProjectError> {
        Ok(self.entries.get(key).cloned())
    }
    fn save(&mut self, key: &str, value: &[u8]) -> Result<(), ProjectError> {
        self.entries.insert(key.to_string(), value.to_vec());
        Ok(())
    }
}
pub fn is_project_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some(PROJECT_EXTENSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_through_memory_store() {
        let manifest = ProjectManifest::new("Replay demo");
        let mut store = MemoryProjectStore::default();
        store
            .save("demo.caplproj", &encode(&manifest).unwrap())
            .unwrap();
        let loaded = decode(&store.load("demo.caplproj").unwrap().unwrap()).unwrap();
        assert_eq!(loaded.name, "Replay demo");
        assert!(is_project_path(Path::new("demo.caplproj")));
    }

    #[test]
    fn rejects_unknown_project_format_versions() {
        let error = decode(
            br#"{"format_version":99,"name":"future","source_files":[],"traces":[],"databases":[],"active_source":null}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("format version 99"));

        let mut manifest = ProjectManifest::new("invalid");
        manifest.format_version = 0;
        assert!(encode(&manifest).is_err());

        let partial = decode(br#"{"format_version":1,"name":"partial"}"#).unwrap();
        assert_eq!(partial.name, "partial");
        assert!(partial.source_files.is_empty());
    }

    #[test]
    fn filesystem_store_rejects_paths_outside_root() {
        let mut store = FsProjectStore::new(std::env::temp_dir().join("capl-project-test"));
        assert!(store.save("../escape.caplproj", b"{}").is_err());
        assert!(store.save("C:\\escape.caplproj", b"{}").is_err());
    }
}
