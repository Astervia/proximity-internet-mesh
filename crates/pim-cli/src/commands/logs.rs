use anyhow::{bail, Result};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_logs(
    log_file: PathBuf,
    no_follow: bool,
    follow_name: bool,
    retry: bool,
    lines: usize,
    no_timestamp: bool,
    since: Option<String>,
    until: Option<String>,
) -> Result<()> {
    let since_prefix = since.as_deref().map(parse_time_spec).transpose()?;
    let until_prefix = until.as_deref().map(parse_time_spec).transpose()?;
    let follow = !no_follow && until_prefix.is_none();

    // Wait for the file if --retry
    let file = match std::fs::File::open(&log_file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if retry {
                eprintln!("pim logs: waiting for {}", log_file.display());
                loop {
                    std::thread::sleep(Duration::from_millis(250));
                    match std::fs::File::open(&log_file) {
                        Ok(f) => break f,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot open log file: {}", log_file.display()))),
                    }
                }
            } else {
                bail!(
                    "log file not found: {} (start with `pim up --detach`, or pass --retry to wait)",
                    log_file.display()
                );
            }
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot open log file: {}", log_file.display()))),
    };
    let mut file_inode = inode_of(&file);
    let mut reader = BufReader::new(file);

    // Collect existing lines so we can honour -n (last N)
    let mut existing: Vec<String> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        existing.push(line.clone());
    }

    let start = if lines == 0 {
        0
    } else {
        existing.len().saturating_sub(lines)
    };

    let mut stop = false;
    for l in &existing[start..] {
        stop = emit_line(l, &since_prefix, &until_prefix, no_timestamp);
        if stop {
            break;
        }
    }

    if !follow || stop {
        return Ok(());
    }

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            if follow_name {
                // Check whether the file has been rotated (inode changed or gone)
                let new_inode = log_file.metadata().ok().map(|m| inode_from_meta(&m));
                if new_inode != file_inode {
                    // Reopen — wait briefly if the new file isn't there yet
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(new_file) = std::fs::File::open(&log_file) {
                        file_inode = inode_of(&new_file);
                        reader = BufReader::new(new_file);
                    }
                    continue;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        if emit_line(&line, &since_prefix, &until_prefix, no_timestamp) {
            break;
        }
    }

    Ok(())
}

/// Print a log line after applying time filters and optional timestamp stripping.
/// Returns `true` when the `--until` boundary has been crossed (caller should stop).
pub(crate) fn emit_line(
    line: &str,
    since_prefix: &Option<String>,
    until_prefix: &Option<String>,
    no_timestamp: bool,
) -> bool {
    let ts = line_timestamp(line);

    if let Some(ref s) = since_prefix {
        if ts.as_deref() < Some(s.as_str()) {
            return false;
        }
    }
    if let Some(ref u) = until_prefix {
        if ts.as_deref() > Some(u.as_str()) {
            return true;
        }
    }

    if no_timestamp {
        // Skip past the timestamp token and one run of whitespace
        let body = line
            .find(|c: char| c.is_ascii_whitespace())
            .map(|i| line[i..].trim_start())
            .unwrap_or(line);
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    } else {
        print!("{line}");
    }

    false
}

/// Extract the first 19 chars of the RFC3339 timestamp at the start of a tracing log line.
/// Tracing format: `2026-04-18T23:42:22.570856Z  INFO target: message`
pub(crate) fn line_timestamp(line: &str) -> Option<String> {
    let ts = line.split_ascii_whitespace().next()?;
    if ts.len() >= 19 && ts.contains('T') {
        Some(ts[..19].to_string())
    } else {
        None
    }
}

/// Parse a time spec into a comparable RFC3339 prefix (first 19 chars, UTC).
/// Accepts RFC3339 strings or relative durations: 30s, 5m, 2h, 1d, 1h30m.
pub(crate) fn parse_time_spec(spec: &str) -> Result<String> {
    if let Some(offset_secs) = parse_relative_duration(spec) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        return Ok(unix_secs_to_datetime_prefix(
            now.saturating_sub(offset_secs),
        ));
    }
    if spec.len() >= 19 && spec.contains('T') {
        return Ok(spec[..19].to_string());
    }
    bail!(
        "invalid time spec {:?} — use RFC3339 (2026-04-18T23:00:00) or relative (5m, 1h30m, 2d)",
        spec
    )
}

/// Parse a relative duration string like `5m`, `1h30m`, `2d` into seconds.
pub(crate) fn parse_relative_duration(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut total = 0u64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            total += match c {
                's' => n,
                'm' => n * 60,
                'h' => n * 3600,
                'd' => n * 86400,
                _ => return None,
            };
        }
    }
    if !num.is_empty() {
        return None; // trailing digits without a unit
    }
    (total > 0).then_some(total)
}

/// Convert a Unix timestamp (seconds) to a `YYYY-MM-DDTHH:MM:SS` string for comparison.
pub(crate) fn unix_secs_to_datetime_prefix(secs: u64) -> String {
    let time_of_day = secs % 86400;
    let days = secs / 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    // Gregorian calendar from days-since-epoch (algorithm by Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };

    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}")
}

#[cfg(unix)]
pub(crate) fn inode_of(file: &std::fs::File) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|m| m.ino())
}

#[cfg(not(unix))]
pub(crate) fn inode_of(_file: &std::fs::File) -> Option<u64> {
    None
}

#[cfg(unix)]
pub(crate) fn inode_from_meta(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
pub(crate) fn inode_from_meta(_meta: &std::fs::Metadata) -> u64 {
    0
}
