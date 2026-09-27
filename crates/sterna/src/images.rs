//! Explicit user-selected attachments; no model tool gets filesystem access here.
use crate::contract::Block;
use base64::Engine;
use std::io::Read;
use std::path::Path;

pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

pub fn load(root: &Path, path: &Path) -> Result<Block, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let path = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve image {}: {error}", path.display()))?;
    let metadata = path
        .metadata()
        .map_err(|error| format!("cannot inspect image: {error}"))?;
    if !metadata.is_file() {
        return Err("image attachment must be a regular file".into());
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err("image attachment exceeds 5 MiB".into());
    }
    let file = std::fs::File::open(&path).map_err(|error| format!("cannot open image: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read image: {error}"))?;
    from_bytes(&bytes)
}

pub fn from_bytes(bytes: &[u8]) -> Result<Block, String> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("image attachment exceeds 5 MiB".into());
    }
    let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return Err("unsupported image signature; attach PNG, JPEG, GIF, or WebP".into());
    };
    Ok(Block::Image {
        media_type: media_type.into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}
