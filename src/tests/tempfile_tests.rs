use crate::tempfile_helper::TempDirGuard;
use std::fs;

// Single end-to-end test for `TempDirGuard`'s two possible outcomes on
// `Drop`, exercised via its `keep_by_default` constructor argument (see
// `tempfile_helper.rs`): the directory is deleted when the guard drops
// unless the caller asked to keep it up front.

#[test]
fn temp_dir_is_removed_unless_kept() {
    let parent = std::env::temp_dir();
    // keep=false: the directory should exist while the guard is alive...
    let removed_path;
    {
        let guard = TempDirGuard::new(Some(&parent), false).unwrap();
        removed_path = guard.path().to_path_buf();
        assert!(removed_path.exists());
    }
    // ...and be gone as soon as it drops, with no explicit cleanup call
    // needed from this test -- `Drop`'s default (non-keep) branch handles
    // the recursive delete on its own.
    assert!(!removed_path.exists());

    // keep=true: same shape, but this time the directory must survive
    // past the guard's drop, since `keep_by_default` was set at
    // construction time (as opposed to being set later via the `keep()`
    // method, which isn't exercised by this test).
    let kept_path;
    {
        let guard = TempDirGuard::new(Some(&parent), true).unwrap();
        kept_path = guard.path().to_path_buf();
    }

    // Since nothing else will clean this directory up (that's the whole
    // point of `keep`), the test tidies up after itself here so repeated
    // runs don't accumulate leftover directories under the system temp
    // dir. Failure to remove it is not treated as a test failure (`let _
    // =`), since it doesn't affect what this test is actually checking.
    assert!(kept_path.exists());
    let _ = fs::remove_dir_all(kept_path);
}