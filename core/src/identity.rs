//! An id for the image itself: xxh3-128 over the JPEG data without the metadata segments, so
//! it survives a metadata write, a rename and a move, and changes when the pixels do.

use xxhash_rust::xxh3::Xxh3;

const SOI: u8 = 0xd8;
const EOI: u8 = 0xd9;
const SOS: u8 = 0xda;
const COMMENT: u8 = 0xfe;

pub fn content_id(bytes: &[u8]) -> Option<String> {
    let mut hasher = Xxh3::new();
    for piece in image_data(bytes)? {
        hasher.update(piece);
    }
    Some(format!("{:032x}", hasher.digest128()))
}

/// Everything but the metadata: APPn and comment segments are skipped, the rest of the file
/// from the first real segment to the end of image is kept.
fn image_data(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != SOI {
        return None;
    }
    let mut pieces = Vec::new();
    let mut at = 2;

    loop {
        while bytes.get(at) == Some(&0xff) && bytes.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        if bytes.get(at) != Some(&0xff) {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            SOS => {
                let end = last_end_of_image(bytes, at).unwrap_or(bytes.len());
                pieces.push(bytes.get(at..end)?);
                return Some(pieces);
            }
            EOI => return Some(pieces),
            0xd0..=0xd8 | 0x01 => at += 2,
            _ => {
                let length = u16::from_be_bytes([*bytes.get(at + 2)?, *bytes.get(at + 3)?]) as usize;
                if length < 2 {
                    return None;
                }
                let segment = bytes.get(at..at + 2 + length)?;
                if !matches!(marker, 0xe0..=0xef | COMMENT) {
                    pieces.push(segment);
                }
                at += 2 + length;
            }
        }
    }
}

/// Anything after the last end-of-image marker (a multi-picture appendix, for example) is not
/// image data of this photo.
fn last_end_of_image(bytes: &[u8], from: usize) -> Option<usize> {
    (from..bytes.len().saturating_sub(1))
        .rev()
        .find(|at| bytes[*at] == 0xff && bytes[at + 1] == EOI)
        .map(|at| at + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(app_payload: &[u8], pixels: &[u8]) -> Vec<u8> {
        let mut out = vec![0xff, SOI];
        out.extend([0xff, 0xe1]);
        out.extend(((app_payload.len() + 2) as u16).to_be_bytes());
        out.extend(app_payload);
        out.extend([0xff, 0xdb]);
        out.extend(4u16.to_be_bytes());
        out.extend([0x00, 0x11]);
        out.extend([0xff, SOS]);
        out.extend(4u16.to_be_bytes());
        out.extend([0x00, 0x01]);
        out.extend(pixels);
        out.extend([0xff, EOI]);
        out
    }

    #[test]
    fn ignores_the_metadata_segments() {
        let one = jpeg(b"Exif small", b"same pixels");
        let other = jpeg(b"Exif considerably longer with tags", b"same pixels");
        assert_ne!(one, other);
        assert_eq!(content_id(&one), content_id(&other));
    }

    #[test]
    fn changes_with_the_pixels() {
        let one = jpeg(b"Exif", b"pixels one");
        let other = jpeg(b"Exif", b"pixels two");
        assert_ne!(content_id(&one), content_id(&other));
    }

    #[test]
    fn ignores_what_follows_the_end_of_image() {
        let plain = jpeg(b"Exif", b"pixels");
        let mut with_appendix = plain.clone();
        with_appendix.extend(b"a multi picture appendix");
        assert_eq!(content_id(&plain), content_id(&with_appendix));
    }

    #[test]
    fn refuses_what_is_not_a_jpeg() {
        assert_eq!(content_id(b"not an image at all"), None);
        assert_eq!(content_id(&[]), None);
        assert_eq!(content_id(&[0xff, SOI]), None);
    }

    /// The real thing: a metadata write must not move the id, and our notion of image data
    /// must be the same as ExifTool's.
    #[test]
    fn survives_a_metadata_write_on_a_real_photo() {
        let dir = std::env::temp_dir().join("photomanager-identity");
        std::fs::create_dir_all(&dir).unwrap();
        let before = dir.join("before.jpg");
        let after = dir.join("after.jpg");
        std::fs::write(&before, include_bytes!("fixtures/p01.jpg")).unwrap();
        std::fs::copy(&before, &after).unwrap();

        let written = std::process::Command::new("exiftool")
            .args(["-overwrite_original", "-q", "-TagsList=places/inChina/Beijing"])
            .args([
                "-DateTimeOriginal=2006:09:14 10:12:00",
                "-GPSLatitude=39.9",
                "-GPSLatitudeRef=N",
            ])
            .arg(&after)
            .status()
            .unwrap();
        assert!(written.success());
        assert_ne!(std::fs::read(&before).unwrap(), std::fs::read(&after).unwrap());

        let id = |path: &std::path::Path| content_id(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(id(&before), id(&after), "metadata moved the id");
        assert_eq!(
            image_hash(&before),
            image_hash(&after),
            "ExifTool disagrees with itself"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn image_hash(path: &std::path::Path) -> String {
        let out = std::process::Command::new("exiftool")
            .args([
                "-s3",
                "-api",
                "RequestAll=3",
                "-api",
                "ImageHashType=SHA256",
                "-ImageDataHash",
            ])
            .arg(path)
            .output()
            .unwrap();
        let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(!hash.is_empty(), "exiftool gave no image data hash");
        hash
    }

    #[test]
    fn reads_as_hex() {
        let id = content_id(&jpeg(b"Exif", b"pixels")).unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
