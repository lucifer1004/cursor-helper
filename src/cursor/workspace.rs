//! Workspace storage operations
//!
//! Cursor stores workspace state in:
//! ~/Library/Application Support/Cursor/User/workspaceStorage/<hash>/
//!
//! The hash is computed as: MD5(absolutePath + Math.round(birthtimeMs))

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use url::Url;

/// Compute the workspace storage hash for a given path
///
/// Formula: MD5(absolutePath + Math.round(birthtimeMs))
///
/// # Arguments
/// * `path` - The absolute path to the project directory
///
/// # Returns
/// The MD5 hash as a hex string
pub fn compute_workspace_hash<P: AsRef<Path>>(path: P) -> Result<String> {
    let path = path.as_ref();
    let path_str = normalize_path_for_hash(path);

    // Get file metadata to extract birth time
    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to get metadata for: {}", path.display()))?;

    // Get birth time (creation time) in milliseconds
    let birthtime_ms = get_birthtime_ms(&metadata)?;

    // Round to nearest integer (like JavaScript's Math.round)
    let birthtime_rounded = birthtime_ms.round() as u64;

    // Compute MD5 hash of "path + birthtimeMs"
    let input = format!("{}{}", path_str, birthtime_rounded);
    let hash = md5::compute(input.as_bytes());

    Ok(format!("{:x}", hash))
}

/// Normalize path for hash computation
/// On Windows, Cursor uses lowercase drive letters (c: not C:)
fn normalize_path_for_hash(path: &Path) -> String {
    let path_str = path.to_string_lossy();

    #[cfg(windows)]
    {
        // Lowercase the drive letter (C: -> c:)
        if path_str.len() >= 2 && path_str.as_bytes()[1] == b':' {
            let mut chars: Vec<char> = path_str.chars().collect();
            chars[0] = chars[0].to_ascii_lowercase();
            return chars.into_iter().collect();
        }
    }

    path_str.into_owned()
}

/// Get birth time in milliseconds from file metadata
#[cfg(target_os = "macos")]
fn get_birthtime_ms(metadata: &fs::Metadata) -> Result<f64> {
    use std::time::UNIX_EPOCH;
    let created = metadata.created().context("Failed to get creation time")?;
    let duration = created
        .duration_since(UNIX_EPOCH)
        .context("Time went backwards")?;
    Ok(duration.as_secs_f64() * 1000.0)
}

#[cfg(target_os = "linux")]
fn get_birthtime_ms(metadata: &fs::Metadata) -> Result<f64> {
    use std::os::unix::fs::MetadataExt;
    // Linux often doesn't have true birthtime. We use statx() birthtime if available,
    // otherwise fall back to ctime. Note: ctime changes on metadata updates, so this
    // may not match Cursor's hash. Use find_workspace_by_uri() as fallback.
    if let Ok(created) = metadata.created() {
        use std::time::UNIX_EPOCH;
        let duration = created
            .duration_since(UNIX_EPOCH)
            .context("Time went backwards")?;
        return Ok(duration.as_secs_f64() * 1000.0);
    }
    // Fallback to ctime (inode change time) - may not match Cursor's hash
    let ctime_sec = metadata.ctime();
    let ctime_nsec = metadata.ctime_nsec();
    Ok((ctime_sec as f64 * 1000.0) + (ctime_nsec as f64 / 1_000_000.0))
}

#[cfg(windows)]
fn get_birthtime_ms(metadata: &fs::Metadata) -> Result<f64> {
    use std::time::UNIX_EPOCH;
    let created = metadata.created().context("Failed to get creation time")?;
    let duration = created
        .duration_since(UNIX_EPOCH)
        .context("Time went backwards")?;
    Ok(duration.as_secs_f64() * 1000.0)
}

/// The workspace.json file structure
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceJson {
    pub folder: String,
}

impl WorkspaceJson {
    /// Create a new workspace.json for a given path
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let url = Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("Failed to convert path to URL: {}", path.display()))?;

        Ok(Self {
            folder: url.to_string(),
        })
    }

    /// Read workspace.json from a file
    #[allow(dead_code)]
    pub fn read<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read: {}", path.as_ref().display()))?;
        serde_json::from_str(&content).context("Failed to parse workspace.json")
    }

    /// Write workspace.json to a file
    pub fn write<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path.as_ref(), content)
            .with_context(|| format!("Failed to write: {}", path.as_ref().display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn test_workspace_json_new() {
        let ws = WorkspaceJson::new("/Users/me/projects/myapp").unwrap();
        assert_eq!(ws.folder, "file:///Users/me/projects/myapp");
    }

    #[cfg(not(windows))]
    #[test]
    fn test_workspace_json_with_spaces() {
        let ws = WorkspaceJson::new("/Users/me/my project").unwrap();
        assert_eq!(ws.folder, "file:///Users/me/my%20project");
    }

    #[cfg(windows)]
    #[test]
    fn test_workspace_json_new_windows() {
        let ws = WorkspaceJson::new("C:\\Users\\me\\projects\\myapp").unwrap();
        assert_eq!(ws.folder, "file:///C:/Users/me/projects/myapp");
    }

    #[cfg(windows)]
    #[test]
    fn test_workspace_json_with_spaces_windows() {
        let ws = WorkspaceJson::new("C:\\Users\\me\\my project").unwrap();
        assert_eq!(ws.folder, "file:///C:/Users/me/my%20project");
    }

    #[cfg(windows)]
    #[test]
    fn test_normalize_path_for_hash_windows() {
        use std::path::Path;
        // Windows: drive letter should be lowercased
        assert_eq!(
            normalize_path_for_hash(Path::new("C:\\com.github\\project")),
            "c:\\com.github\\project"
        );
        assert_eq!(
            normalize_path_for_hash(Path::new("D:\\Users\\me")),
            "d:\\Users\\me"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_normalize_path_for_hash_unix() {
        use std::path::Path;
        // Unix: path unchanged
        assert_eq!(
            normalize_path_for_hash(Path::new("/Users/me/project")),
            "/Users/me/project"
        );
    }
}
