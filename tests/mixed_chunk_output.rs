use std::fs;
use std::process::Command;

fn scan(data: &[u8], encodings: &str, chunk: usize, string_only: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    fs::write(&input, data).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_pstrings"));
    command.args(["-e", encodings, "-c", &chunk.to_string(), "-j", "2"]);
    if string_only {
        command.arg("-s");
    }
    let result = command.arg(&input).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn mixed_encodings_preserve_strings_and_global_order_across_chunks() {
    for (data, encodings) in [
        ("éABCDéEFGHéIJKL\0".as_bytes().to_vec(), "ascii,utf8"),
        ("AB😀CあDEF\0".as_bytes().to_vec(), "ascii,utf8"),
        (
            "ABCD".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            "ascii,utf16le-ascii",
        ),
    ] {
        for string_only in [false, true] {
            let reference = scan(&data, encodings, data.len(), string_only);
            for chunk in 1..=data.len() {
                assert_eq!(
                    scan(&data, encodings, chunk, string_only),
                    reference,
                    "encodings={encodings}, chunk={chunk}, string_only={string_only}"
                );
            }
        }
    }
}
