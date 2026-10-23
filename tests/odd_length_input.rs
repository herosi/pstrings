use std::fs;
use std::process::Command;

#[test]
fn an_even_chunk_width_accepts_an_odd_length_utf16le_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let mut bytes: Vec<_> = "ABCD".encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.push(0);
    fs::write(&input, bytes).unwrap();
    for size in ["auto", "2", "8", "64", "18446744073709551614"] {
        let result = Command::new(env!("CARGO_BIN_EXE_pstrings"))
            .args(["-e", "utf16le", "-c", size, "-j", "1"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "size={size}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8(result.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "00000000000000000000\tUTF16LE\tABCD\n"
        );
    }
    for size in ["0", "3"] {
        let result = Command::new(env!("CARGO_BIN_EXE_pstrings"))
            .args(["-e", "utf16le", "-c", size])
            .arg(&input)
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
}
