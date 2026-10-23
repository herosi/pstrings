use std::fs;
use std::process::Command;

fn scan(bytes: &[u8], chunk: usize, filters: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("utf16.bin");
    fs::write(&input, bytes).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_pstrings"))
        .args([
            "-e",
            "utf16le",
            "-f",
            filters,
            "-c",
            &chunk.to_string(),
            "-j",
            "2",
        ])
        .arg(&input)
        .output()
        .unwrap();
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
fn parity_selection_uses_complete_runs_at_even_and_odd_offsets() {
    for text in [
        "あいうえお",
        "漢字漢字漢字漢字",
        "ABC漢字DEFあいう",
        "𠀀あ字𠀀B漢字",
    ] {
        for offset in 0..8 {
            let mut bytes = vec![0; offset];
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            bytes.extend([0, 0]);
            if bytes.len() % 2 != 0 {
                bytes.push(0);
            }
            let expected = format!("{offset:020}\tUTF16LE\t{text}\n");
            for chunk in [2, 4, 6, 8, 16, 64] {
                assert_eq!(
                    scan(
                        &bytes,
                        chunk,
                        "ascii,kanji-jis2,hiragana,katakana,kanji-ext-b"
                    ),
                    expected,
                    "text={text}, offset={offset}, chunk={chunk}"
                );
            }
        }
    }
}

#[test]
fn broad_filter_heuristics_are_independent_of_chunk_size() {
    for text in [
        "あいうえお",
        "漢字漢字漢字漢字",
        "ABC漢字DEFあいう",
        "𠀀あ字𠀀B漢字",
    ] {
        for offset in 0..8 {
            let mut bytes = vec![0; offset];
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            bytes.extend([0, 0]);
            if bytes.len() % 2 != 0 {
                bytes.push(0);
            }
            let filters = "ascii,kanji,hiragana,katakana,kanji-ext-b";
            let expected = scan(&bytes, bytes.len(), filters);
            for chunk in [2, 4, 6, 8, 16, 64] {
                assert_eq!(
                    scan(&bytes, chunk, filters),
                    expected,
                    "text={text}, offset={offset}, chunk={chunk}"
                );
            }
        }
    }
}
