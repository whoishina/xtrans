use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Build a script that discovers the foreground shell's current directory.
pub fn cwd_script(session_id: &str) -> String {
    let id = shell_quote(session_id);
    format!(
        r#"id={id}
for p in /proc/[0-9]*; do
    p=${{p##*/}}
    [ -r /proc/$p/environ ] || continue
    tr '\0' '\n' < /proc/$p/environ 2>/dev/null | grep -qx "LC_XTRANS_ID=$id" || continue
    stat=$(cat /proc/$p/stat 2>/dev/null) || continue
    stat=${{stat##*) }}
    set -- $stat
    [ "${{5:-0}}" -ne 0 ] || continue
    shell_pid=$p
    break
done
[ -n "${{shell_pid:-}}" ] || exit 0
stat=$(cat /proc/$shell_pid/stat 2>/dev/null) || exit 0
stat=${{stat##*) }}
set -- $stat
tpgid=${{6:-0}}
if [ "$tpgid" -gt 0 ] 2>/dev/null && [ -r /proc/$tpgid/cwd ]; then
    readlink /proc/$tpgid/cwd 2>/dev/null
else
    readlink /proc/$shell_pid/cwd 2>/dev/null
fi
"#
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Parse the one-line absolute path emitted by the remote script.
pub fn parse_cwd_output(stdout: &[u8]) -> Option<String> {
    let output = std::str::from_utf8(stdout)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    if output.is_empty() || output.contains(['\r', '\n']) || !output.starts_with('/') {
        return None;
    }
    Some(output.to_owned())
}

/// Detect the remote working directory through the existing SSH connection.
pub fn detect_cwd(
    control_path: Option<&str>,
    remote_dest: &str,
    session_id: &str,
) -> Option<String> {
    let control_path = control_path?;
    let mut command = Command::new("ssh");
    command
        .arg("-o")
        .arg(format!("ControlPath={control_path}"))
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(remote_dest)
        .arg("sh")
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(cwd_script(session_id).as_bytes()).is_err()
    {
        let _ = child.kill();
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut output = Vec::new();
                child.stdout.take()?.read_to_end(&mut output).ok()?;
                return parse_cwd_output(&output);
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_and_invalid_cwd_output() {
        assert_eq!(parse_cwd_output(b"/home/u\n"), Some("/home/u".into()));
        assert_eq!(parse_cwd_output(b"relative\n"), None);
        assert_eq!(parse_cwd_output(b"/one\n/two\n"), None);
        assert_eq!(parse_cwd_output(b""), None);
    }

    #[test]
    fn script_quotes_session_id() {
        let script = cwd_script("12-34'evil");
        assert!(script.contains("id='12-34'\\''evil'"));
        assert!(script.contains("LC_XTRANS_ID=$id"));
    }
}
