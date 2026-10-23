use std::fs;
use std::path::Path;
use std::process::Command;

fn scan(input: &Path, encodings: &str, filters: &str, chunk_size: usize, jobs: usize) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_pstrings"))
        .args([
            "-e",
            encodings,
            "-f",
            filters,
            "-c",
            &chunk_size.to_string(),
            "-j",
            &jobs.to_string(),
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn utf16le_ascii_rejects_wide_characters_even_when_the_filter_accepts_them() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("wide.bin");
    // Truncating these code units produces LF, CR, another control byte,
    // or printable ASCII, respectively. None belongs to the ASCII/Latin-1 scanner.
    for text in ["褊褊褊褊", "褍褍褍褍", "丘丘丘丘", "丸丸丸丸"] {
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for prefix in [vec![], vec![0]] {
            let mut data = prefix;
            data.extend_from_slice(&bytes);
            fs::write(&input, data).unwrap();
            for filters in ["kanji-jis2", "ascii,kanji-jis2", "printable"] {
                let actual = scan(&input, "utf16le-ascii", filters, 64, 1);
                assert!(
                    actual.is_empty(),
                    "text={text:?}, filters={filters}: {actual:?}"
                );
            }
        }
    }
}

#[test]
fn combining_utf16le_scanners_preserves_text_across_chunk_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.bin");
    for offset in [0, 1] {
        let mut data = vec![0; offset];
        // U+890B terminates the JIS level 2 run. The four U+890A code
        // units used to produce an extra record containing four newlines.
        data.extend("褊褊褊褊褋".encode_utf16().flat_map(u16::to_le_bytes));
        if data.len() % 2 != 0 {
            data.push(0);
        }
        fs::write(&input, &data).unwrap();
        let expected = format!("{offset:020}\tUTF16LE\t褊褊褊褊\n");
        for chunk_size in [2, 4, 6, 8, 10, 64] {
            for encodings in ["utf16le,utf16le-ascii", "utf16le-ascii,utf16le"] {
                let actual = scan(&input, encodings, "kanji-jis2", chunk_size, 2);
                assert_eq!(
                    actual, expected,
                    "offset={offset}, chunk_size={chunk_size}, encodings={encodings}"
                );
            }
        }
    }
}

#[test]
fn utf16le_ascii_preserves_ascii_and_latin1_with_a_wide_filter() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("latin1.bin");
    let data: Vec<u8> = "ABCDàéîõü\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(&input, data).unwrap();
    for chunk_size in [2, 8, 64] {
        let actual = scan(
            &input,
            "utf16le-ascii",
            "ascii,latin1,kanji-jis2",
            chunk_size,
            2,
        );
        assert_eq!(actual, "00000000000000000000\tUTF16LE\tABCDàéîõü\n");
    }
}
