//! Core Cursor IDE operations

pub mod folder_id;
pub mod storage;
pub mod workspace;

// Re-exports for library consumers
#[allow(unused_imports)]
pub use folder_id::path_to_folder_id;
#[allow(unused_imports)]
pub use workspace::compute_workspace_hash;
