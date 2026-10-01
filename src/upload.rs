use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn sanitize_filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = name
        .chars()
        .filter(|c| *c != '\0' && !c.is_control())
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "xtrans-file".into()
    } else {
        cleaned
    }
}

pub fn upload_script(dir: &str, name: &str) -> String {
    let dir = shell_quote(dir);
    let name = shell_quote(&sanitize_filename(name));
    format!(
        r#"set -C
dir={dir}
mkdir -p "$dir" || exit 1
base={name}
stem=$base
ext=
case "$base" in
  .*|*.*.*|*.) : ;;
  *.*) stem=${{base%.*}}; ext=.${{base##*.}} ;;
esac
i=0
while [ "$i" -le 1000 ]; do
  if [ "$i" -eq 0 ]; then target="$dir/$base"; else target="$dir/$stem-$i$ext"; fi
  if (cat > "$target") 2>/dev/null; then
    printf '%s\n' "$target"
    exit 0
  fi
  i=$((i + 1))
done
exit 1
"#
    )
}

pub fn upload_to_remote<R: Read>(
    control_path: Option<&str>,
    remote_dest: &str,
    remote_command: &str,
    mut data: R,
) -> Option<String> {
    let mut cmd = Command::new("ssh");
    if let Some(cp) = control_path {
        cmd.arg("-o")
            .arg(format!("ControlPath={cp}"))
            .arg("-o")
            .arg("ControlMaster=no");
    }
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg(remote_dest)
        .arg("sh")
        .arg("-c")
        .arg(shell_quote(remote_command))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    {
        let mut stdin = child.stdin.take()?;
        if io::copy(&mut data, &mut stdin).is_err() {
            let _ = child.kill();
            return None;
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim_end_matches(['\r', '\n']);
    (!path.is_empty() && path.starts_with('/') && !path.contains(['\r', '\n']))
        .then_some(path.into())
}

pub fn upload_file(
    control_path: Option<&str>,
    remote_dest: &str,
    dir: &str,
    name: &str,
    file: &Path,
) -> Option<String> {
    let input = File::open(file).ok()?;
    upload_to_remote(control_path, remote_dest, &upload_script(dir, name), input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn quotes_shell_metacharacters() {
        assert_eq!(shell_quote("a'b $x\n"), "'a'\\''b $x\n'");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_filename("a/b\\c.txt"), "c.txt");
        assert_eq!(sanitize_filename("."), "xtrans-file");
        assert_eq!(sanitize_filename("a\0\n"), "a");
    }

    #[cfg(unix)]
    #[test]
    fn local_upload_script_handles_duplicates_and_names() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("xtrans-upload-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let run = |name: &str, content: &[u8]| {
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg(upload_script(dir.to_str().unwrap(), name))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(content).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "status={:?}, stdout={:?}, stderr={:?}",
                output.status,
                output.stdout,
                output.stderr
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let first = run("a.txt", b"one");
        let second = run("a.txt", b"two");
        assert!(first.ends_with("/a.txt"));
        assert!(second.ends_with("/a-1.txt"));
        assert_eq!(fs::read(&first).unwrap(), b"one");
        assert_eq!(fs::read(&second).unwrap(), b"two");
        assert!(run("a space's.txt", b"x").ends_with("/a space's.txt"));
        assert!(run(".bashrc", b"x").ends_with("/.bashrc"));
        assert!(run(".bashrc", b"x").ends_with("/.bashrc-1"));
        assert!(run("name", b"x").ends_with("/name"));
        assert!(run("name", b"x").ends_with("/name-1"));
        let _ = fs::remove_dir_all(dir);
    }
}
