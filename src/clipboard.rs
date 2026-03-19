use arboard::Clipboard;
use image::ImageEncoder;

pub enum Content {
    Image(Vec<u8>),
    Text(String),
    Empty,
}

/// Read clipboard content, prioritizing image over text.
pub fn read() -> Content {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(_) => return Content::Empty,
    };

    // Try image first
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

    // Fall back to text
    if let Ok(text) = clipboard.get_text()
        && !text.is_empty()
    {
        return Content::Text(text);
    }

    Content::Empty
}
