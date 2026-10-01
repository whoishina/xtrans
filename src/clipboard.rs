use std::path::{Path, PathBuf};

use arboard::Clipboard;
use image::ImageEncoder;

#[derive(Debug, PartialEq, Eq)]
pub enum Content {
    Files(Vec<PathBuf>),
    Url(String),
    Image(Vec<u8>),
    Text(String),
    Empty,
}

/// Read clipboard content in file, image, text priority order.
pub fn read() -> Content {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(_) => return Content::Empty,
    };

    if let Ok(paths) = clipboard.get().file_list() {
        let files: Vec<_> = paths.into_iter().filter(|path| path.is_file()).collect();
        if !files.is_empty() {
            return Content::Files(files);
        }
    }

    if let Ok(img_data) = clipboard.get_image() {
        let mut png_buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
        if encoder
            .write_image(
                &img_data.bytes,
                img_data.width as u32,
                img_data.height as u32,
                image::ExtendedColorType::Rgba8,
            )
            .is_ok()
        {
            return Content::Image(png_buf);
        }
    }

    match clipboard.get_text() {
        Ok(text) => {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            classify_text(&text, home.as_deref(), |path| path.is_file())
        }
        Err(_) => Content::Empty,
    }
}

/// Classify clipboard text without accessing the filesystem except through `exists`.
pub fn classify_text(text: &str, home: Option<&Path>, exists: impl Fn(&Path) -> bool) -> Content {
    if text.is_empty() {
        return Content::Empty;
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 1 {
        let paths: Vec<_> = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| path_from_text(line, home, &exists))
            .collect();
        let non_empty = lines.iter().filter(|line| !line.trim().is_empty()).count();
        if non_empty > 0 && paths.len() == non_empty {
            return Content::Files(paths);
        }
        return Content::Text(text.to_owned());
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Content::Empty;
    }
    if let Some(url) = http_url(trimmed) {
        return Content::Url(url.to_owned());
    }
    if let Some(path) = path_from_text(trimmed, home, &exists) {
        return Content::Files(vec![path]);
    }
    Content::Text(text.to_owned())
}

fn http_url(text: &str) -> Option<&str> {
    if (text.starts_with("http://") || text.starts_with("https://"))
        && !text.chars().any(char::is_whitespace)
    {
        Some(text)
    } else {
        None
    }
}

fn path_from_text(
    text: &str,
    home: Option<&Path>,
    exists: &impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let candidate = unquote_path(text.trim());
    if candidate.starts_with("file://") {
        let path = file_uri_path(&candidate)?;
        return exists(&path).then_some(path);
    }
    if let Some(rest) = candidate.strip_prefix("~/") {
        let path = home?.join(rest);
        return exists(&path).then_some(path);
    }
    let path = PathBuf::from(&candidate);
    if (path.is_absolute() || is_windows_absolute(&candidate)) && exists(&path) {
        Some(path)
    } else {
        None
    }
}

fn unquote_path(text: &str) -> String {
    let mut value = text.to_owned();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            value = value[1..value.len() - 1].to_owned();
        }
    }
    value.replace("\\ ", " ")
}

fn is_windows_absolute(text: &str) -> bool {
    text.len() >= 3
        && text.as_bytes()[0].is_ascii_alphabetic()
        && text.as_bytes()[1] == b':'
        && matches!(text.as_bytes()[2], b'\\' | b'/')
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if rest.starts_with('/') {
        rest.to_owned()
    } else {
        return None;
    };
    Some(PathBuf::from(percent_decode(&path)?))
}

pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = (bytes[index + 1] as char).to_digit(16)? as u8;
            let low = (bytes[index + 2] as char).to_digit(16)? as u8;
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn classifier(text: &str, home: Option<&Path>, files: &[&str]) -> Content {
        let files: HashSet<_> = files.iter().copied().collect();
        classify_text(text, home, |path| {
            files.contains(path.to_str().unwrap_or_default())
        })
    }

    #[test]
    fn classifies_file_path_and_expands_home() {
        let home = Path::new("/home/test");
        assert_eq!(
            classifier("/tmp/a.txt", None, &["/tmp/a.txt"]),
            Content::Files(vec![PathBuf::from("/tmp/a.txt")])
        );
        assert_eq!(
            classifier("~/a.txt", Some(home), &["/home/test/a.txt"]),
            Content::Files(vec![PathBuf::from("/home/test/a.txt")])
        );
    }

    #[test]
    fn classifies_file_uri_and_quoted_path() {
        assert_eq!(
            classifier("file:///tmp/a%20b.txt", None, &["/tmp/a b.txt"]),
            Content::Files(vec![PathBuf::from("/tmp/a b.txt")])
        );
        assert_eq!(
            classifier("'/tmp/a\\ b.txt'", None, &["/tmp/a b.txt"]),
            Content::Files(vec![PathBuf::from("/tmp/a b.txt")])
        );
    }

    #[test]
    fn classifies_urls_and_text() {
        assert_eq!(
            classifier("https://example.test/a", None, &[]),
            Content::Url("https://example.test/a".into())
        );
        assert_eq!(
            classifier("https://example.test/a b", None, &[]),
            Content::Text("https://example.test/a b".into())
        );
        assert_eq!(
            classifier("hello", None, &[]),
            Content::Text("hello".into())
        );
    }

    #[test]
    fn classifies_only_all_existing_multiline_paths() {
        assert_eq!(
            classifier("/a\n/b", None, &["/a", "/b"]),
            Content::Files(vec![PathBuf::from("/a"), PathBuf::from("/b")])
        );
        assert_eq!(
            classifier("/a\nhello", None, &["/a"]),
            Content::Text("/a\nhello".into())
        );
        assert_eq!(classifier("\n", None, &[]), Content::Empty);
    }

    #[test]
    fn percent_decode_rejects_invalid_sequences() {
        assert_eq!(percent_decode("a%20b"), Some("a b".into()));
        assert_eq!(percent_decode("%ZZ"), None);
        assert_eq!(classify_text("", None, |_| false), Content::Empty);
    }
}
