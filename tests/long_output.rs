use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::process::Command;

#[test]
fn long_strings_are_preserved_through_spilling_and_ordered_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.bin");
    let output = dir.path().join("output.tsv");
    let mut file = File::create(&input).unwrap();
    let block = vec![b'A'; 1024 * 1024];
    for _ in 0..9 {
        file.write_all(&block).unwrap();
    }
    file.write_all(b"AAAAAAAAAAAAAAAAA").unwrap();
    drop(file);
    let length = fs::metadata(&input).unwrap().len();
    let result = Command::new(env!("CARGO_BIN_EXE_pstrings"))
        .args(["-e", "ascii,utf8", "-j", "1", "-c", "1M", "--temp-dir"])
        .arg(dir.path())
        .arg("-o")
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut reader = BufReader::new(File::open(&output).unwrap());
    for encoding in ["ASCII", "UTF8"] {
        let prefix = format!("00000000000000000000\t{encoding}\t");
        let mut actual = vec![0; prefix.len()];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(actual, prefix.as_bytes());
        let mut remaining = length;
        let mut bytes = [0; 8192];
        while remaining > 0 {
            let count = remaining.min(bytes.len() as u64) as usize;
            reader.read_exact(&mut bytes[..count]).unwrap();
            assert!(bytes[..count].iter().all(|&b| b == b'A'));
            remaining -= count as u64;
        }
        let ending: &[u8] = if cfg!(windows) { b"\r\n" } else { b"\n" };
        let mut actual = vec![0; ending.len()];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(actual, ending);
    }
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
}
