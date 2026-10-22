//! milkfs's answers in words, for `disks`: the errnos its control files give, the reason
//! line in /System/Kernel/fs/milkfs/error, and the progress lines of a scrub (`disks verify`)
//! and of an encrypt / decrypt in place.

use alloc::format;
use alloc::string::String;

/// What a milkfs control write's errno means for `disks`.
pub fn why(errno: i32) -> String {
    let text = match errno {
        2 => {
            "there's no snapshot or clone by that name (for a receive: the stream's base snapshot isn't on the target)"
        }
        1 => "needs an administrator's authorisation: run it as `auth disks ...` (or as root)",
        5 => "a disk error, or a damaged stream",
        13 => "wrong passphrase or recovery key",
        16 => {
            "busy: a clone is built on that snapshot, a send is reading it, or (receive) the target changed since the base snapshot: --force rolls it back"
        }
        39 => "a full stream needs an empty volume",
        18 => "that isn't a milkfs volume",
        17 => "that name is taken",
        19 => "there's no milkfs volume",
        22 => "that name or setting can't be used",
        28 => "no room: 30 snapshots and clones at most",
        30 => "the volume is read-only",
        36 => "the name is too long (47 characters at most)",
        e => return format!("failed (error {e})"),
    };
    format!("{text} (error {errno})")
}

/// The kernel's reason for control `op`'s last failure, from the error file's
/// `op: ENAME: reason` line, if that line is about `op`.
pub fn kernel_reason(op: &str, error_file: &str, errno: i32) -> Option<String> {
    let rest = error_file.trim_end().strip_prefix(op)?.strip_prefix(": ")?;
    let (name, reason) = rest.split_once(": ")?;
    Some(format!("{reason} ({name}, error {errno})"))
}

/// The run number in a scrub status line (`[run 3] running ...`), 0 without one.
pub fn scrub_run(status: &str) -> u64 {
    status
        .split("[run ")
        .nth(1)
        .and_then(|x| x.split(']').next())
        .and_then(|x| x.parse().ok())
        .unwrap_or(0)
}

/// The percentage in a running scrub's line (`... 120/480 blocks (25%) ...`).
pub fn scrub_percent(line: &str) -> Option<u64> {
    line.split("blocks (")
        .nth(1)
        .and_then(|x| x.split('%').next())
        .and_then(|x| x.parse::<u64>().ok())
}

/// The percentage in an encrypt / decrypt progress line (`vda: converting: encrypt 40% ...`).
pub fn convert_percent(line: &str) -> Option<u64> {
    line.split(": converting: ")
        .nth(1)
        .and_then(|x| x.split('%').next())
        .and_then(|x| x.split(' ').nth(1))
        .and_then(|x| x.parse::<u64>().ok())
}

/// Did a scrub's final report find bad blocks?
pub fn scrub_found_errors(report: &str) -> bool {
    report.contains("\nerror:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(why(17), "that name is taken (error 17)");
        assert!(why(1).contains("`auth disks ...`"));
        assert_eq!(why(9999), "failed (error 9999)");
        assert_eq!(
            kernel_reason("clone", "clone: EBUSY: the snapshot is being sent\n", 16).as_deref(),
            Some("the snapshot is being sent (EBUSY, error 16)")
        );
        assert_eq!(kernel_reason("delete", "clone: EBUSY: x\n", 16), None);
    }

    #[test]
    fn progress() {
        assert_eq!(scrub_run("[run 3] running: 120/480 blocks (25%)"), 3);
        assert_eq!(scrub_run("idle"), 0);
        assert_eq!(
            scrub_percent("[run 3] running: 120/480 blocks (25%)"),
            Some(25)
        );
        assert_eq!(
            convert_percent("vda: converting: encrypt 40% (2/5 MB)"),
            Some(40)
        );
        assert_eq!(convert_percent("vda: encrypted"), None);
        assert!(scrub_found_errors("[run 1] done\nerror: block 7"));
        assert!(!scrub_found_errors("[run 1] done\n"));
    }
}
