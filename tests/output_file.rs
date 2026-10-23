use std::fs;
use std::path::Path;
use std::process::Command;

fn extract(input: &Path, output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pstrings"))
        .args(["-e", "ascii", "-j", "1", "-o"])
        .arg(output)
        .arg(input)
        .output()
        .unwrap()
}

#[test]
fn output_cannot_truncate_the_input_or_a_hard_link() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let alias = dir.path().join("alias.bin");
    let bytes = b"HELLO WORLD\0";
    fs::write(&input, bytes).unwrap();
    fs::hard_link(&input, &alias).unwrap();
    for output in [&input, &alias] {
        let result = extract(&input, output);
        assert!(!result.status.success());
        assert_eq!(fs::read(&input).unwrap(), bytes);
        assert!(String::from_utf8_lossy(&result.stderr).contains("same file"));
    }
}

#[cfg(unix)]
#[test]
fn output_cannot_truncate_the_input_through_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let alias = dir.path().join("alias.bin");
    let bytes = b"HELLO WORLD\0";
    fs::write(&input, bytes).unwrap();
    std::os::unix::fs::symlink(&input, &alias).unwrap();
    let result = extract(&input, &alias);
    assert!(!result.status.success());
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert!(String::from_utf8_lossy(&result.stderr).contains("same file"));
}

#[test]
fn a_distinct_output_file_is_created_or_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let output = dir.path().join("output.txt");
    let bytes = b"HELLO WORLD\0";
    fs::write(&input, bytes).unwrap();
    for previous in [
        None,
        Some("previous output that is longer than the new output".repeat(8)),
    ] {
        if let Some(previous) = previous {
            fs::write(&output, previous).unwrap();
        }
        assert!(extract(&input, &output).status.success());
        assert_eq!(fs::read(&input).unwrap(), bytes);
        assert_eq!(
            fs::read_to_string(&output).unwrap().replace("\r\n", "\n"),
            "00000000000000000000\tASCII\tHELLO WORLD\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_regular_outputs_still_accept_writes_and_report_write_errors() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    fs::write(&input, b"HELLO WORLD\0").unwrap();
    assert!(extract(&input, Path::new("/dev/null")).status.success());
    if Path::new("/dev/full").exists() {
        let result = extract(&input, Path::new("/dev/full"));
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("No space left"));
    }
}
