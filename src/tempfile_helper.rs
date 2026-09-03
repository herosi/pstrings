use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// RAII wrapper around a `tempfile::TempDir` (a whole scratch directory,
/// as opposed to `create_temp_file` below which manages individual files).
/// By default the directory and everything in it is recursively deleted
/// when this guard drops; setting `keep` (either at construction or via
/// `keep()`) disables that cleanup so the directory survives the process,
/// which is useful for debugging a run after the fact (e.g. a `--keep-temp`
/// CLI flag).
pub struct TempDirGuard {
    // `Option` so `Drop` and `keep()` can each independently take ownership
    // of the underlying `TempDir` without a borrow-checker conflict --
    // whichever runs first "consumes" it, and the other becomes a no-op.
    dir: Option<tempfile::TempDir>,
    // The caller's default from construction time, consulted only by
    // `Drop` when `keep()` was never called explicitly. Immutable after
    // construction -- calling `keep()` decides the outcome directly (by
    // taking `dir`) rather than by mutating this field, so this always
    // reflects what the caller originally asked for, nothing more.
    keep_by_default: bool,
}

impl TempDirGuard {
    /// Creates a fresh temporary directory, either inside `parent` (so all
    /// of this run's temp files/dirs share one filesystem, which matters
    /// since temp files can't be renamed/persisted across filesystems) or
    /// in the OS default temp location if `parent` is `None`.
    pub fn new(parent: Option<&Path>, keep: bool) -> io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("pstrings-");
        let dir = match parent {
            Some(parent) => builder.tempdir_in(parent)?,
            None => builder.tempdir()?,
        };
        Ok(Self {
            dir: Some(dir),
            keep_by_default: keep,
        })
    }

    pub fn path(&self) -> &Path {
        self.dir.as_ref().unwrap().path()
    }

    /// Consumes the guard and returns the directory's path, having
    /// disabled automatic cleanup -- the caller is now responsible for the
    /// directory (or is content to leave it on disk, e.g. for inspection).
    ///
    /// Returns `io::Result` only for signature compatibility with earlier
    /// versions of `tempfile`; it cannot actually fail today.
    pub fn keep(mut self) -> io::Result<PathBuf> {
        let dir = self.dir.take().unwrap();
        Ok(dir.keep())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        // `dir` is `Some` here only if `keep()` was never called -- if it
        // was, `dir` is already `None` and there's nothing left for Drop
        // to do (no risk of trying to "keep" the same directory twice).
        // Otherwise, fall back to whatever the caller asked for at
        // construction time: `TempDir` performs recursive cleanup on drop
        // by default, and calling `dir.keep()` disables that.
        if let Some(dir) = self.dir.take() {
            if self.keep_by_default {
                let _ = dir.keep();
            }
        }
    }
}

/// Creates a fresh temp file at (a name derived from) `path`.
///
/// If `keep` is
/// false, the file is unlinked from the filesystem immediately after
/// creation (readable/writable through the returned handle until it is
/// dropped) so callers never need to remember to delete it themselves.
/// If `keep` is true the file is left in place under its derived name, so
/// it can be inspected after the run -- this is what `--keep-temp` does.
pub(crate) fn create_temp_file(path: &Path, keep: bool) -> io::Result<File> {
    let dirname = path.parent().unwrap();
    let filename = path.file_name().unwrap();

    // Named after `filename` (so, e.g., a file derived from a chunk's temp
    // path is easy to recognize on disk if `keep` is set) but created in
    // the same directory as `path` so it can later be renamed/persisted
    // atomically (renames across filesystems aren't atomic, or possible at
    // all in some cases).
    let file = tempfile::Builder::new().prefix(filename).tempfile_in(dirname)?;

    if keep {
        // Rename the temp file to a permanent name in place and hand back
        // a plain `File` -- the caller is responsible for it from here on
        // (matches `TempDirGuard::keep`'s "caller now owns this" contract).
        let (file, _path) = file.keep()?;
        return Ok(file);
    }

    // `into_file()` unlinks the directory entry immediately (classic
    // create-then-unlink trick for a self-cleaning file) while keeping the
    // returned `File` handle fully readable/writable. Since nothing else
    // ever references this file by path, the space it uses is reclaimed by
    // the OS as soon as the handle is dropped -- no explicit delete call,
    // and no risk of leaking it even on a crash or early return.
    Ok(file.into_file())
}
