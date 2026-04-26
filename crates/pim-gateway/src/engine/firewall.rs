//! Host firewall and command helpers for gateway setup.

use super::GatewayError;

#[cfg(target_os = "linux")]
pub(super) fn input_drop_args<'a>(proto: &'a str, iface: &'a str) -> [&'a str; 10] {
    [
        "-A",
        "INPUT",
        "-i",
        iface,
        "-p",
        proto,
        "--dport",
        "30000:59999",
        "-j",
        "DROP",
    ]
}

pub(crate) fn run_cmd(program: &str, args: &[&str]) -> Result<(), GatewayError> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(GatewayError::CommandFailed(format!(
            "{program} {} exited with {:?}",
            args.join(" "),
            status.code()
        )))
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn run_cmd_with_stdin(
    program: &str,
    args: &[&str],
    stdin: &str,
) -> Result<(), GatewayError> {
    use std::io::Write;

    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    if let Some(mut child_stdin) = child.stdin.take() {
        child_stdin
            .write_all(stdin.as_bytes())
            .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;
    }

    let status = child
        .wait()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(GatewayError::CommandFailed(format!(
            "{program} {} exited with {:?}",
            args.join(" "),
            status.code()
        )))
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn check_cmd_quiet(program: &str, args: &[&str]) -> Result<bool, GatewayError> {
    let status = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    Ok(status.success())
}

/// Repeatedly run `<program> -D ...` until the matching rule is absent.
/// `add_args` is the `-A ...` form (or `-t nat -A ...` for NAT rules); we
/// transform `-A`→`-D` for delete and `-A`→`-C` for presence check. Bounded
/// to defend against wedged iptables processes and historical duplicates.
#[cfg(target_os = "linux")]
pub(crate) fn iptables_delete_if_present(program: &str, add_args: &[&str]) {
    let replace = |op: &'static str| -> Vec<&str> {
        let mut v: Vec<&str> = add_args.to_vec();
        if let Some(pos) = v.iter().position(|a| *a == "-A") {
            v[pos] = op;
        }
        v
    };
    for _ in 0..8 {
        let check = replace("-C");
        let status = std::process::Command::new(program)
            .args(&check)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let present = matches!(status, Ok(s) if s.success());
        if !present {
            return;
        }
        let del = replace("-D");
        let _ = std::process::Command::new(program)
            .args(&del)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}
