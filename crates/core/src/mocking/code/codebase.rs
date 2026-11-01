use super::contracts::{Assembly, Target};
use std::path::{Path, PathBuf};

/// Provides existing codebase files without requiring a filesystem implementation.
pub trait ExistingFiles {
    /// Reads a UTF-8 file at a path relative to the codebase root.
    fn read(&self, path: &Path) -> std::io::Result<Option<String>>;

    /// Checks whether a path exists, including files that need not be read.
    fn exists(&self, path: &Path) -> std::io::Result<bool> {
        self.read(path).map(|contents| contents.is_some())
    }
}

/// Application-supplied codebase root and access to existing files.
pub struct CodebaseContext<'a> {
    pub root: &'a Path,
    pub files: &'a dyn ExistingFiles,
}

/// Resolved codebase contents, ready to inspect or write.
///
/// Paths must be nonempty, unique within the plan, and consist only of normal
/// relative path components. Absolute paths and parent components are rejected.
/// The filesystem writer checks destinations and removals before changing any files.
pub struct CodebasePlan {
    /// Obsolete files to remove only if their contents still match the planned snapshot.
    pub removals: Vec<CodebaseRemoval>,
    pub files: Vec<CodebaseFile>,
}

pub struct CodebaseFile {
    /// The output path relative to the codebase root.
    pub path: PathBuf,
    pub contents: String,
    pub slugs: Vec<String>,
}

/// Resolves a target's codebase files against application-supplied existing files.
///
/// This delegates to [`Target::codebase`] without invoking the filesystem writer.
/// Reading existing files may fail through the supplied [`ExistingFiles`]
/// implementation. The target's concrete error is returned unchanged.
pub fn plan_codebase<T: Target + ?Sized>(
    assembly: &Assembly,
    target: &T,
    context: &CodebaseContext<'_>,
) -> Result<CodebasePlan, T::Error> {
    target.codebase(assembly, context)
}

/// An obsolete file identified from the caller's existing-file snapshot.
pub struct CodebaseRemoval {
    pub path: PathBuf,
    /// The writer preserves a file changed since planning.
    pub expected: String,
}
