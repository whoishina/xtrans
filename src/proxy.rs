use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clipboard;
use crate::remote;
use crate::terminal::Terminal;
use crate::upload;

pub fn run(ssh_args: &[String]) {
    let remote_dest = parse_ssh_destination(ssh_args).unwrap_or_else(|| {
        eprintln!("Cannot determine SSH destination from arguments");
        std::process::exit(1);
    });

    let session_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );

    // ControlMaster multiplexing for fast uploads — Unix only.
    // Windows/MSYS2 lacks Unix domain socket fd-passing required by OpenSSH
    // ControlMaster, causing "mm_receive_fd" errors.
    let control_path = if cfg!(unix) {
        Some(
            std::env::temp_dir()
                .join(format!("xtrans-ctrl-{}", std::process::id()))
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    };

    let mut term = Terminal::new().unwrap_or_else(|e| {
        eprintln!("Terminal setup failed: {e}");
        std::process::exit(1);
    });

    let mut cmd = Command::new("ssh");
    cmd.arg("-tt");
    if let Some(ref cp) = control_path {
        cmd.arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg(format!("ControlPath={cp}"));
    }
    if cfg!(unix) {
        cmd.arg("-o")
            .arg(format!("SetEnv=LC_XTRANS_ID={session_id}"));
    }
    term.configure_ssh_command(&mut cmd);
    let mut child = cmd
        .args(ssh_args)
        .stdin(term.ssh_stdin())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| {
            term.cleanup();
            eprintln!("Failed to start ssh: {e}");
            std::process::exit(1);
        });

    let writer = term.finalize(&mut child);

    let ctrl = control_path.clone();
    let dest = remote_dest.clone();

    let sid = session_id.clone();
    std::thread::spawn(move || {
        input_loop(writer, ctrl.as_deref(), &dest, &sid);
    });

    let status = child.wait().unwrap_or_else(|e| {
        term.cleanup();
        eprintln!("SSH error: {e}");
        std::process::exit(1);
    });

    term.cleanup();
    if let Some(ref cp) = control_path {
        let _ = std::fs::remove_file(cp);
    }
    std::process::exit(status.code().unwrap_or(1));
}

// ---------------------------------------------------------------------------
// Input proxy
// ---------------------------------------------------------------------------

fn input_loop(
    mut ssh_stdin: Box<dyn Write + Send>,
    control_path: Option<&str>,
    remote_dest: &str,
    session_id: &str,
) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut buf = [0u8; 4096];

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };

        if forward_input(
            &buf[..n],
            &mut *ssh_stdin,
            control_path,
            remote_dest,
            session_id,
        )
        .is_err()
        {
            break;
        }
    }
}

/// Forward input bytes to SSH, intercepting Ctrl+V (0x16).
fn forward_input(
    input: &[u8],
    ssh_stdin: &mut dyn Write,
    control_path: Option<&str>,
    remote_dest: &str,
    session_id: &str,
) -> io::Result<()> {
    forward_input_with(input, ssh_stdin, |out| {
        handle_paste(out, control_path, remote_dest, session_id);
    })
}

/// Core byte-forwarding logic — calls `on_paste` for every Ctrl+V found.
fn forward_input_with(
    input: &[u8],
    ssh_stdin: &mut dyn Write,
    mut on_paste: impl FnMut(&mut dyn Write),
) -> io::Result<()> {
    let mut start = 0;

    for i in 0..input.len() {
        if input[i] == 0x16 {
            if i > start {
                ssh_stdin.write_all(&input[start..i])?;
            }
            on_paste(ssh_stdin);
            start = i + 1;
        }
    }

    if start < input.len() {
        ssh_stdin.write_all(&input[start..])?;
    }
    ssh_stdin.flush()
}

// ---------------------------------------------------------------------------
// Clipboard paste handler
// ---------------------------------------------------------------------------

fn handle_paste(
    ssh_stdin: &mut (impl Write + ?Sized),
    control_path: Option<&str>,
    remote_dest: &str,
    session_id: &str,
) {
    match clipboard::read() {
        clipboard::Content::Image(png_data) => {
            let ts = unix_timestamp();
            let remote_path = format!("/tmp/xtrans-{ts}.png");
            if let Some(path) = upload::upload_to_remote(
                control_path,
                remote_dest,
                &format!(
                    "cat > {} && printf '%s\\n' {}",
                    upload::shell_quote(&remote_path),
                    upload::shell_quote(&remote_path)
                ),
                png_data.as_slice(),
            ) {
                write_remote_paths(ssh_stdin, &[path]);
            }
        }
        clipboard::Content::Files(files) => {
            let dir = remote::detect_cwd(control_path, remote_dest, session_id)
                .unwrap_or_else(|| format!("/tmp/xtrans-{}", unix_timestamp()));
            let mut paths = Vec::new();
            for file in files {
                let name = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("xtrans-file");
                match upload::upload_file(control_path, remote_dest, &dir, name, &file) {
                    Some(path) => paths.push(path),
                    None => eprintln!("\r\nFailed to upload {}", file.display()),
                }
            }
            write_remote_paths(ssh_stdin, &paths);
        }
        clipboard::Content::Url(url) => match download_url(&url) {
            Some((path, name)) => {
                let dir = remote::detect_cwd(control_path, remote_dest, session_id)
                    .unwrap_or_else(|| format!("/tmp/xtrans-{}", unix_timestamp()));
                let result = upload::upload_file(control_path, remote_dest, &dir, &name, &path);
                let _ = std::fs::remove_file(path);
                if let Some(remote_path) = result {
                    write_remote_paths(ssh_stdin, &[remote_path]);
                }
            }
            None => paste_text(ssh_stdin, &url),
        },
        clipboard::Content::Text(text) => paste_text(ssh_stdin, &text),
        clipboard::Content::Empty => {
            let _ = ssh_stdin.write_all(&[0x16]);
            let _ = ssh_stdin.flush();
        }
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn paste_text(ssh_stdin: &mut (impl Write + ?Sized), text: &str) {
    let _ = ssh_stdin.write_all(b"\\x1b[200~");
    let _ = ssh_stdin.write_all(text.as_bytes());
    let _ = ssh_stdin.write_all(b"\\x1b[201~");
    let _ = ssh_stdin.flush();
}

fn write_remote_paths(ssh_stdin: &mut (impl Write + ?Sized), paths: &[String]) {
    let rendered = paths
        .iter()
        .map(|path| {
            if path.bytes().all(|byte| {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._/+-@%:,="
                    .contains(&byte)
            }) {
                path.clone()
            } else {
                upload::shell_quote(path)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !rendered.is_empty() {
        let _ = ssh_stdin.write_all(rendered.as_bytes());
        let _ = ssh_stdin.flush();
    }
}

fn download_url(url: &str) -> Option<(std::path::PathBuf, String)> {
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .call()
        .ok()?;
    let disposition = response
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok());
    let name = filename_from_url(url, disposition);
    let path = std::env::temp_dir().join(format!("xtrans-dl-{}-{name}", unix_timestamp()));
    let reader = response.into_body().into_reader();
    let mut file = File::create(&path).ok()?;
    let mut limited = reader.take(200 * 1024 * 1024 + 1);
    io::copy(&mut limited, &mut file).ok()?;
    if file.metadata().ok()?.len() > 200 * 1024 * 1024 {
        let _ = std::fs::remove_file(&path);
        eprintln!("\r\nDownloaded URL exceeds 200 MB");
        return None;
    }
    Some((path, name))
}

fn filename_from_url(url: &str, disposition: Option<&str>) -> String {
    let candidate = disposition.and_then(|header| {
        header.split(';').map(str::trim).find_map(|part| {
            let lower = part.to_ascii_lowercase();
            if lower.starts_with("filename*=") {
                let value = part.split_once('=')?.1.trim();
                let value = value
                    .strip_prefix("UTF-8''")
                    .or_else(|| value.strip_prefix("utf-8''"))?;
                clipboard::percent_decode(value)
            } else if lower.starts_with("filename=") {
                Some(part.split_once('=')?.1.trim_matches(['"', '\'']).to_owned())
            } else {
                None
            }
        })
    });
    let candidate = candidate.or_else(|| {
        url.split(['?', '#'])
            .next()?
            .rsplit('/')
            .find(|part| !part.is_empty())
            .and_then(clipboard::percent_decode)
    });
    upload::sanitize_filename(candidate.as_deref().unwrap_or("download")).to_owned()
}

// ---------------------------------------------------------------------------
// SSH argument parser — extract destination (user@host)
// ---------------------------------------------------------------------------

fn parse_ssh_destination(args: &[String]) -> Option<String> {
    const OPTS_WITH_ARG: &[char] = &[
        'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'L', 'l', 'm', 'O', 'o', 'p', 'Q', 'R', 'S',
        'W', 'w',
    ];

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            return args.get(i + 1).cloned();
        }

        if arg.starts_with('-') && arg.len() >= 2 {
            let flag = arg.chars().nth(1).unwrap();
            if OPTS_WITH_ARG.contains(&flag) {
                i += if arg.len() == 2 { 2 } else { 1 };
            } else {
                i += 1;
            }
        } else {
            return Some(arg.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // -- parse_ssh_destination -------------------------------------------

    #[test]
    fn simple_destination() {
        assert_eq!(
            parse_ssh_destination(&args(&["ubuntu@10.0.0.1"])),
            Some("ubuntu@10.0.0.1".into())
        );
    }

    #[test]
    fn destination_with_flags() {
        assert_eq!(
            parse_ssh_destination(&args(&["-t", "-v", "root@host"])),
            Some("root@host".into())
        );
    }

    #[test]
    fn destination_with_option_value() {
        assert_eq!(
            parse_ssh_destination(&args(&["-p", "2222", "-i", "key.pem", "deploy@srv"])),
            Some("deploy@srv".into())
        );
    }

    #[test]
    fn destination_with_combined_option() {
        assert_eq!(
            parse_ssh_destination(&args(&["-p2222", "user@host"])),
            Some("user@host".into())
        );
    }

    #[test]
    fn destination_after_double_dash() {
        assert_eq!(
            parse_ssh_destination(&args(&["-v", "--", "user@host"])),
            Some("user@host".into())
        );
    }

    #[test]
    fn no_destination() {
        assert_eq!(parse_ssh_destination(&args(&["-v", "-t"])), None);
    }

    // -- forward_input (Ctrl+V interception) -----------------------------

    fn noop_paste(_out: &mut dyn Write) {}

    fn marker_paste(out: &mut dyn Write) {
        let _ = out.write_all(b"<IMG>");
    }

    #[test]
    fn passthrough_normal_bytes() {
        let mut out = Vec::new();
        forward_input_with(b"hello", &mut out, noop_paste).unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn ctrl_v_triggers_paste_handler() {
        let mut out = Vec::new();
        forward_input_with(&[0x16], &mut out, marker_paste).unwrap();
        assert_eq!(out, b"<IMG>");
    }

    #[test]
    fn bytes_around_ctrl_v() {
        let mut out = Vec::new();
        forward_input_with(&[0x41, 0x16, 0x42], &mut out, marker_paste).unwrap();
        assert_eq!(out, b"A<IMG>B");
    }

    #[test]
    fn multiple_ctrl_v() {
        let mut out = Vec::new();
        forward_input_with(&[0x16, 0x16], &mut out, marker_paste).unwrap();
        assert_eq!(out, b"<IMG><IMG>");
    }

    #[test]
    fn no_ctrl_v_no_paste() {
        let mut count = 0u32;
        let mut out = Vec::new();
        forward_input_with(b"normal text", &mut out, |_| count += 1).unwrap();
        assert_eq!(count, 0);
        assert_eq!(out, b"normal text");
    }
}
