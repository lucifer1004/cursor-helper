//! Folder ID computation for ~/.cursor/projects/
//!
//! Cursor stores project-specific data in ~/.cursor/projects/<folder-id>/
//! where folder-id is derived from the absolute path by replacing / and . with -
//! and removing the leading -.

use std::path::Path;

/// Convert an absolute path to a Cursor folder ID
///
/// Cursor slugifies paths by:
/// 1. Replacing both `/` and `.` with `-`
/// 2. Collapsing consecutive `-` into single `-`
/// 3. Trimming leading/trailing `-`
///
/// # Example
/// ```
/// use cursor_helper::cursor::folder_id::path_to_folder_id;
///
/// let id = path_to_folder_id("/Users/me/.claude");
/// assert_eq!(id, "Users-me-claude");
/// ```
pub fn path_to_folder_id<P: AsRef<Path>>(path: P) -> String {
    let path_str = path.as_ref().to_string_lossy();

    // Replace / and . with -, then collapse consecutive dashes
    let slugified = path_str.replace(['/', '.'], "-");

    // Collapse consecutive dashes and trim
    let mut result = String::with_capacity(slugified.len());
    let mut prev_dash = false;

    for c in slugified.chars() {
        if c == '-' {
            if !prev_dash && !result.is_empty() {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }

    result.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_path() {
        assert_eq!(
            path_to_folder_id("/Users/me/projects/myapp"),
            "Users-me-projects-myapp"
        );
    }

    #[test]
    fn test_root_path() {
        assert_eq!(path_to_folder_id("/"), "");
    }

    #[test]
    fn test_nested_path() {
        assert_eq!(
            path_to_folder_id("/home/user/dev/rust/cursor-helper"),
            "home-user-dev-rust-cursor-helper"
        );
    }

    #[test]
    fn test_path_with_dots() {
        // Cursor replaces both / and . with -
        assert_eq!(
            path_to_folder_id("/Users/me/com.example/my-project"),
            "Users-me-com-example-my-project"
        );
    }

    #[test]
    fn test_hidden_folder() {
        // Hidden folders like .claude: /. becomes single -
        assert_eq!(path_to_folder_id("/Users/me/.claude"), "Users-me-claude");
    }

    #[test]
    fn test_multiple_dots() {
        // Multiple consecutive dots/slashes collapse
        assert_eq!(path_to_folder_id("/Users/me/../foo"), "Users-me-foo");
    }
}
