//! Small pictures of the photos, keyed by the content id so a move or a metadata write never
//! invalidates one. Disposable: everything here can be made again from the photos.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fast_image_resize::images::Image as Canvas;
use fast_image_resize::{PixelType, Resizer};
use turbojpeg::{Decompressor, Image, PixelFormat};

const QUALITY: f32 = 80.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// What the grid draws.
    Small,
    /// A bigger cell or a quick look.
    Large,
}

impl Size {
    pub const ALL: [Size; 2] = [Size::Small, Size::Large];

    pub fn pixels(self) -> u32 {
        match self {
            Size::Small => 256,
            Size::Large => 512,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Size::Small => "256",
            Size::Large => "512",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// Not a picture we can make a thumbnail of.
    Unusable(String),
    /// Not a content id, so we refuse to build a path from it.
    NotAnId(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unusable(why) => write!(f, "{why}"),
            Error::NotAnId(id) => write!(f, "{id} is not a content id"),
            Error::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The store on disk: `<root>/<size>/<first two hex digits>/<content id>.webp`.
#[derive(Debug, Clone)]
pub struct Thumbs {
    root: PathBuf,
}

impl Thumbs {
    pub fn new(root: impl Into<PathBuf>) -> Thumbs {
        Thumbs { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, content_id: &str, size: Size) -> Result<PathBuf> {
        if content_id.len() < 4 || !content_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::NotAnId(content_id.to_string()));
        }
        Ok(self
            .root
            .join(size.as_str())
            .join(&content_id[..2])
            .join(format!("{content_id}.webp")))
    }

    pub fn has(&self, content_id: &str, size: Size) -> bool {
        self.path(content_id, size).is_ok_and(|path| path.is_file())
    }

    pub fn load(&self, content_id: &str, size: Size) -> Option<Vec<u8>> {
        std::fs::read(self.path(content_id, size).ok()?).ok()
    }

    /// Makes and stores the thumbnail unless it is already there. True when it encoded one.
    pub fn store(&self, content_id: &str, size: Size, jpeg: &[u8], orientation: Option<i64>) -> Result<bool> {
        let target = self.path(content_id, size)?;
        if target.is_file() {
            return Ok(false);
        }
        let picture = make(jpeg, size.pixels(), orientation)?;
        write_atomically(&target, &picture)?;
        Ok(true)
    }

    pub fn forget(&self, content_id: &str) -> Result<()> {
        for size in Size::ALL {
            let path = self.path(content_id, size)?;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub fn count(&self, size: Size) -> usize {
        walk(&self.root.join(size.as_str())).count()
    }

    /// Drops every thumbnail no photo points at any more.
    pub fn sweep(&self, keep: &HashSet<String>) -> Result<usize> {
        let mut removed = 0;
        for size in Size::ALL {
            for path in walk(&self.root.join(size.as_str())) {
                let id = path.file_stem().map(|stem| stem.to_string_lossy().to_string());
                if id.is_some_and(|id| keep.contains(&id)) {
                    continue;
                }
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// JPEG bytes in, WebP bytes out: decoded at the smallest scale the DCT offers, resized to the
/// wanted longest side, turned the right way up. Never larger than the original.
pub fn make(jpeg: &[u8], pixels: u32, orientation: Option<i64>) -> Result<Vec<u8>> {
    let decoded = match fast(jpeg, pixels) {
        Ok(decoded) => decoded,
        // libjpeg-turbo refuses a few files that every viewer opens. They are worth a second,
        // slower try rather than a hole in the grid.
        Err(refused) => lenient(jpeg).map_err(|_| refused)?,
    };

    let longest = decoded.full_width.max(decoded.full_height);
    let wanted = pixels.min(longest);
    let (width, height) = fit(decoded.full_width, decoded.full_height, wanted);

    let (pixels, width, height) = if decoded.width == width && decoded.height == height {
        (decoded.pixels, width, height)
    } else {
        let source = Canvas::from_vec_u8(decoded.width, decoded.height, decoded.pixels, PixelType::U8x3)
            .map_err(|error| Error::Unusable(error.to_string()))?;
        let mut small = Canvas::new(width, height, PixelType::U8x3);
        Resizer::new()
            .resize(&source, &mut small, None)
            .map_err(|error| Error::Unusable(error.to_string()))?;
        (small.into_vec(), width, height)
    };

    let (pixels, width, height) = turn(pixels, width, height, orientation.unwrap_or(1));
    Ok(webp::Encoder::from_rgb(&pixels, width, height).encode(QUALITY).to_vec())
}

/// The picture as red, green and blue at whatever size the decoder gave us, and how big the
/// photo really is.
struct Decoded {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    full_width: u32,
    full_height: u32,
}

/// libjpeg-turbo, decoding straight to roughly the size we want. A 24 megapixel photo never
/// exists in memory at full size.
fn fast(jpeg: &[u8], wanted: u32) -> Result<Decoded> {
    let mut decompressor = Decompressor::new().map_err(unusable)?;
    let header = decompressor.read_header(jpeg).map_err(unusable)?;
    if header.width == 0 || header.height == 0 {
        return Err(Error::Unusable("no image data".to_string()));
    }

    let longest = header.width.max(header.height);
    let wanted = (wanted as usize).min(longest);
    let factor = Decompressor::supported_scaling_factors()
        .into_iter()
        .filter(|factor| factor.scale(longest) >= wanted)
        .min_by_key(|factor| factor.scale(longest))
        .unwrap_or(turbojpeg::ScalingFactor::ONE);
    decompressor.set_scaling_factor(factor).map_err(unusable)?;

    let scaled = header.scaled(factor);
    let mut image = Image {
        pixels: vec![0u8; scaled.width * scaled.height * 3],
        width: scaled.width,
        pitch: scaled.width * 3,
        height: scaled.height,
        format: PixelFormat::RGB,
    };
    decompressor.decompress(jpeg, image.as_deref_mut()).map_err(unusable)?;

    Ok(Decoded {
        pixels: image.pixels,
        width: scaled.width as u32,
        height: scaled.height as u32,
        full_width: header.width as u32,
        full_height: header.height as u32,
    })
}

/// The second try: a decoder that minds less about how the file is put together. It cannot
/// scale while decoding, so it reads the whole picture.
fn lenient(jpeg: &[u8]) -> Result<Decoded> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(jpeg));
    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB));
    let pixels = decoder.decode().map_err(unusable)?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| Error::Unusable("no image data".to_string()))?;

    Ok(Decoded {
        pixels,
        width: width as u32,
        height: height as u32,
        full_width: width as u32,
        full_height: height as u32,
    })
}

fn unusable<E: std::fmt::Display>(error: E) -> Error {
    Error::Unusable(error.to_string())
}

/// The size with the same shape whose longest side is `wanted`.
fn fit(width: u32, height: u32, wanted: u32) -> (u32, u32) {
    let scale = f64::from(wanted) / f64::from(width.max(height));
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// The eight ways EXIF says a picture can sit in its file.
fn turn(pixels: Vec<u8>, width: u32, height: u32, orientation: i64) -> (Vec<u8>, u32, u32) {
    if !(2..=8).contains(&orientation) {
        return (pixels, width, height);
    }
    let (out_width, out_height) = match orientation {
        5..=8 => (height, width),
        _ => (width, height),
    };
    let (w, h) = (width as usize, height as usize);
    let mut turned = vec![0u8; pixels.len()];

    for y in 0..out_height as usize {
        for x in 0..out_width as usize {
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let from = (sy * w + sx) * 3;
            let to = (y * out_width as usize + x) * 3;
            turned[to..to + 3].copy_from_slice(&pixels[from..from + 3]);
        }
    }
    (turned, out_width, out_height)
}

fn write_atomically(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, target)
}

fn walk(dir: &Path) -> impl Iterator<Item = PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "webp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: usize, height: usize) -> Vec<u8> {
        let mut pixels = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 3;
                pixels[at] = (x * 255 / width.max(1)) as u8;
                pixels[at + 1] = (y * 255 / height.max(1)) as u8;
                pixels[at + 2] = 128;
            }
        }
        let image = Image {
            pixels: pixels.as_slice(),
            width,
            pitch: width * 3,
            height,
            format: PixelFormat::RGB,
        };
        turbojpeg::compress(image, 90, turbojpeg::Subsamp::Sub2x2)
            .unwrap()
            .to_vec()
    }

    fn measure(webp: &[u8]) -> (u32, u32) {
        let header = webp::BitstreamFeatures::new(webp).expect("a webp picture");
        (header.width(), header.height())
    }

    fn store(name: &str) -> Thumbs {
        let root = std::env::temp_dir().join(format!("photomanager-thumbs-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        Thumbs::new(root)
    }

    #[test]
    fn the_longest_side_becomes_the_wanted_size() {
        let wide = make(&jpeg(1600, 900), 256, None).unwrap();
        assert_eq!(measure(&wide), (256, 144));

        let tall = make(&jpeg(900, 1600), 256, None).unwrap();
        assert_eq!(measure(&tall), (144, 256));

        let large = make(&jpeg(1600, 900), 512, None).unwrap();
        assert_eq!(measure(&large), (512, 288));
    }

    #[test]
    fn a_small_photo_is_never_blown_up() {
        let small = make(&jpeg(120, 90), 256, None).unwrap();
        assert_eq!(measure(&small), (120, 90));
    }

    #[test]
    fn every_orientation_comes_out_the_right_way_up() {
        let photo = jpeg(1600, 900);
        for orientation in 1..=8 {
            let thumbnail = make(&photo, 256, Some(orientation)).unwrap();
            let expected = match orientation {
                5..=8 => (144, 256),
                _ => (256, 144),
            };
            assert_eq!(measure(&thumbnail), expected, "orientation {orientation}");
        }
    }

    #[test]
    fn a_turn_moves_the_pixels_it_should() {
        // A 2x1 picture: red, then green.
        let pixels = vec![255, 0, 0, 0, 255, 0];
        let (upright, width, height) = turn(pixels.clone(), 2, 1, 1);
        assert_eq!((upright, width, height), (pixels.clone(), 2, 1));

        let (mirrored, ..) = turn(pixels.clone(), 2, 1, 2);
        assert_eq!(mirrored, vec![0, 255, 0, 255, 0, 0]);

        // Rotated 90 degrees clockwise the red pixel sits at the top.
        let (rotated, width, height) = turn(pixels, 2, 1, 6);
        assert_eq!((width, height), (1, 2));
        assert_eq!(rotated, vec![255, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn a_stored_thumbnail_is_found_by_its_content_id() {
        let thumbs = store("found");
        let id = "0123456789abcdef0123456789abcdef";
        assert!(!thumbs.has(id, Size::Small));

        assert!(thumbs.store(id, Size::Small, &jpeg(800, 600), None).unwrap());
        assert!(thumbs.has(id, Size::Small));
        assert!(!thumbs.has(id, Size::Large), "the other size is its own file");
        assert_eq!(measure(&thumbs.load(id, Size::Small).unwrap()), (256, 192));
        assert_eq!(thumbs.count(Size::Small), 1);
    }

    #[test]
    fn a_second_call_does_not_encode_again() {
        let thumbs = store("again");
        let id = "abcdef0123456789abcdef0123456789";
        let photo = jpeg(800, 600);
        assert!(thumbs.store(id, Size::Small, &photo, None).unwrap());

        let path = thumbs.path(id, Size::Small).unwrap();
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!thumbs.store(id, Size::Small, &photo, None).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), written);
    }

    #[test]
    fn the_forgiving_decoder_reads_the_same_picture() {
        let photo = jpeg(800, 600);
        let decoded = lenient(&photo).unwrap();
        assert_eq!((decoded.full_width, decoded.full_height), (800, 600));
        assert_eq!(decoded.pixels.len(), 800 * 600 * 3);
    }

    #[test]
    fn a_damaged_photo_is_an_error_not_a_panic() {
        assert!(matches!(make(b"", 256, None), Err(Error::Unusable(_))));
        assert!(matches!(make(b"not a jpeg at all", 256, None), Err(Error::Unusable(_))));

        let mut cut = jpeg(800, 600);
        cut.truncate(40);
        assert!(make(&cut, 256, None).is_err());

        let thumbs = store("damaged");
        let id = "ffffffffffffffffffffffffffffffff";
        assert!(thumbs.store(id, Size::Small, b"rubbish", None).is_err());
        assert!(!thumbs.has(id, Size::Small), "nothing is left behind");
    }

    #[test]
    fn a_path_is_only_built_from_a_content_id() {
        let thumbs = store("paths");
        for wrong in ["../../escape", "/etc/passwd", "", "zz"] {
            assert!(matches!(thumbs.path(wrong, Size::Small), Err(Error::NotAnId(_))));
            assert!(thumbs.store(wrong, Size::Small, &jpeg(80, 60), None).is_err());
        }
    }

    #[test]
    fn writes_nothing_outside_its_own_directory() {
        let thumbs = store("contained");
        let id = "1234567890abcdef1234567890abcdef";
        thumbs.store(id, Size::Small, &jpeg(800, 600), None).unwrap();
        thumbs.store(id, Size::Large, &jpeg(800, 600), None).unwrap();

        for size in Size::ALL {
            for path in walk(&thumbs.root().join(size.as_str())) {
                assert!(path.starts_with(thumbs.root()), "{} escaped", path.display());
            }
        }
        assert_eq!(thumbs.count(Size::Small), 1);
        assert_eq!(thumbs.count(Size::Large), 1);
    }

    #[test]
    fn sweeping_keeps_what_the_photos_point_at() {
        let thumbs = store("sweep");
        let kept = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let dropped = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for id in [kept, dropped] {
            for size in Size::ALL {
                thumbs.store(id, size, &jpeg(400, 300), None).unwrap();
            }
        }

        let keep: HashSet<String> = [kept.to_string()].into_iter().collect();
        assert_eq!(thumbs.sweep(&keep).unwrap(), 2, "both sizes of the stale one");
        assert!(thumbs.has(kept, Size::Small));
        assert!(!thumbs.has(dropped, Size::Small));

        thumbs.forget(kept).unwrap();
        assert_eq!(thumbs.count(Size::Small), 0);
        assert_eq!(thumbs.count(Size::Large), 0);
    }
}
