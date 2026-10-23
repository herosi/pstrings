#![cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;

#[test]
fn an_expensive_first_chunk_does_not_exhaust_file_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("uneven.bin");
    let mut file = File::create(&input).unwrap();
    let unit: Vec<u8> = "ABCD\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
    file.write_all(&unit.repeat(1048576 / unit.len() + 1)[..1048576])
        .unwrap();
    file.set_len(256 * 1048576).unwrap();
    drop(file);
    for jobs in ["1", "4"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pstrings"));
        command
            .args(["-e", "utf16le", "-c", "1M", "-j", jobs, "-o", "/dev/null"])
            .arg(&input);
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 64,
                    rlim_max: 64,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "jobs={jobs}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
