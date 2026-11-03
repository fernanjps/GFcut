use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

use crate::{error::TimelineError, model::Project};

impl Project {
    /// Serializes the project to pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String, TimelineError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Deserializes a project from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self, TimelineError> {
        Ok(serde_json::from_str(json)?)
    }

    /// Loads a project from a `.gfcut.json` file on disk.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, TimelineError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| TimelineError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_json_str(&contents)
    }

    /// Atomically writes the project to `path`.
    ///
    /// Writes to a temporary file in the same directory first, flushes it, and atomically
    /// renames it over `path`. This guarantees that file watchers in `apps/desktop` (GPUI)
    /// or concurrent MCP readers never observe a truncated or partially-written JSON file.
    pub fn save_to_file_atomic(&self, path: impl AsRef<Path>) -> Result<(), TimelineError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));

        fs::create_dir_all(parent).map_err(|source| TimelineError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let mut json = self.to_json_pretty()?;
        json.push('\n');

        let mut temp_file = NamedTempFile::new_in(parent).map_err(|source| TimelineError::Io {
            path: PathBuf::from(parent),
            source,
        })?;

        temp_file
            .write_all(json.as_bytes())
            .and_then(|()| temp_file.flush())
            .map_err(|source| TimelineError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        temp_file.persist(path).map_err(|err| TimelineError::Io {
            path: path.to_path_buf(),
            source: err.error,
        })?;

        Ok(())
    }
}
