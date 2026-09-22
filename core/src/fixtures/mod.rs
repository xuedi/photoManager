//! A small stand-in library with the shapes the real one has: events without a full date, a
//! loose file in a country folder, sub-folders, photos without GPS but with a place tag, a
//! photo without any date, XMP dates that disagree with EXIF, and one without tags at all.
//!
//! Only for tests and for looking at the application without touching real photos.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;
use std::process::Command;

const GRAY: &[u8] = include_bytes!("gray.jpg");
const RED: &[u8] = include_bytes!("red.jpg");
const BLUE: &[u8] = include_bytes!("blue.jpg");

struct Photo {
    path: &'static str,
    image: &'static [u8],
    metadata: &'static [&'static str],
}

const PLACES_CHINA: &str = "-TagsList=places/inChina/Beijing";
const PLACES_DENMARK: &str = "-TagsList=places/inDenmark/Copenhagen";

const PHOTOS: &[Photo] = &[
    Photo {
        path: "China/IMG_3140.JPG",
        image: GRAY,
        metadata: &["-DateTimeOriginal=2006:09:14 10:12:00", "-Model=Canon PowerShot A640"],
    },
    Photo {
        path: "China/2006-09-00 Besuch Ben/P1000001.JPG",
        image: RED,
        metadata: &[
            "-DateTimeOriginal=2006:08:21 09:30:00",
            "-Model=Panasonic DMC-LS1",
            PLACES_CHINA,
            "-TagsList=timeline/2006",
            "-TagsList=events/2006 Besuch Ben",
        ],
    },
    Photo {
        path: "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG",
        image: BLUE,
        metadata: &[
            "-DateTimeOriginal=2006:08:21 16:45:00",
            "-Model=Panasonic DMC-LS1",
            PLACES_CHINA,
            "-TagsList=people/groupChina/Ben",
        ],
    },
    Photo {
        path: "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG",
        image: GRAY,
        metadata: &["-TagsList=places/inChina", "-TagsList=mixed/food"],
    },
    Photo {
        path: "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG",
        image: RED,
        metadata: &[
            "-DateTimeOriginal=2018:10:06 14:02:11",
            "-Model=X100S",
            "-Make=FUJIFILM",
            PLACES_DENMARK,
            "-TagsList=events/2018 Wedding Trip",
        ],
    },
    Photo {
        path: "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG",
        image: BLUE,
        metadata: &["-DateTimeOriginal=2018:10:06 14:03:40", "-Model=X100S"],
    },
    Photo {
        path: "Germany/2019-07-13 Sommerfest/img_0657.jpg",
        image: GRAY,
        metadata: &[
            "-DateTimeOriginal=2019:07:13 18:20:00",
            "-Orientation#=6",
            "-GPSLatitude=53.5511",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9937",
            "-GPSLongitudeRef=E",
            "-TagsList=places/inGermany/Hamburg",
        ],
    },
    Photo {
        path: "Germany/2019-07-13 Sommerfest/p1000003.jpg",
        image: RED,
        metadata: &[
            "-DateTimeOriginal=2019:07:13 19:05:00",
            "-XMP-xmp:CreateDate=2019:07:13 17:05:00Z",
            "-XMP-xmp:Label=timeline",
            "-TagsList=places/inGermany/Hamburg",
        ],
    },
    Photo {
        path: "Germany/2019-07-13 Sommerfest/IMAG0001.jpg",
        image: BLUE,
        metadata: &[
            "-DateTimeOriginal=2019:07:13 20:41:00",
            "-TagsList=people/family/Anna",
            "-TagsList=people/me",
            "-TagsList=places/inGermany",
        ],
    },
    Photo {
        path: "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG",
        image: GRAY,
        metadata: &[
            "-DateTimeOriginal=2008:10:03 11:15:00",
            "-TagsList=places/inIreland/Galway",
        ],
    },
    Photo {
        path: "Ireland/2008-10-03 Galway/IMG_0003.JPG",
        image: RED,
        metadata: &[
            "-DateTimeOriginal=2008:10:03 12:47:00",
            "-TagsList=places/inNetherland/Amsterdam",
            "-TagsList=mixed/Funny",
        ],
    },
    Photo {
        path: "Greece/0000-00-00 Aeron ilands/IMG_0004.JPG",
        image: BLUE,
        metadata: &["-TagsList=places/inGreece/athens", "-TagsList=mixed/discusting"],
    },
];

/// Shotwell wrote every tag path into the hierarchical fields and its leaf into the flat ones.
fn flat_tags(metadata: &[&str]) -> Vec<String> {
    metadata
        .iter()
        .filter_map(|arg| arg.strip_prefix("-TagsList="))
        .flat_map(|path| {
            let leaf = path.rsplit('/').next().unwrap_or(path);
            [
                format!("-XMP-microsoft:LastKeywordXMP={path}"),
                format!("-XMP-dc:Subject={leaf}"),
                format!("-IPTC:Keywords={leaf}"),
            ]
        })
        .collect()
}

pub fn photo_count() -> usize {
    PHOTOS.len()
}

pub fn photo_paths() -> Vec<&'static str> {
    PHOTOS.iter().map(|photo| photo.path).collect()
}

/// Writes the stand-in library into `root`, replacing whatever is there.
pub fn build(root: &Path) -> Result<()> {
    refuse_real_photos(root)?;
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }

    for photo in PHOTOS {
        let file = root.join(photo.path);
        std::fs::create_dir_all(file.parent().expect("a parent directory"))?;
        std::fs::write(&file, photo.image)?;

        let status = Command::new("exiftool")
            .arg("-overwrite_original")
            .arg("-q")
            .args(photo.metadata)
            .args(flat_tags(photo.metadata))
            .arg(&file)
            .status()
            .map_err(|error| Error::new(ErrorKind::NotFound, format!("run exiftool: {error}")))?;
        if !status.success() {
            return Err(Error::other(format!("exiftool failed on {}", photo.path)));
        }
    }
    Ok(())
}

/// The real library is never a target, whatever a caller passes in.
fn refuse_real_photos(root: &Path) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let forbidden = Path::new(&home).join("Nextcloud");
    if !home.is_empty() && root.starts_with(&forbidden) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{} is inside the real photo library", root.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_build_inside_the_real_library() {
        let home = std::env::var("HOME").unwrap();
        let error = build(&Path::new(&home).join("Nextcloud/Photos")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn builds_every_photo_with_its_metadata() {
        let root = std::env::temp_dir().join("photomanager-fixture-test");
        build(&root).unwrap();

        for path in photo_paths() {
            let file = root.join(path);
            assert!(file.is_file(), "{path} is missing");
            assert!(file.metadata().unwrap().len() > 0, "{path} is empty");
        }

        let tagged = Command::new("exiftool")
            .args(["-s3", "-TagsList", &root.join(PHOTOS[1].path).to_string_lossy()])
            .output()
            .unwrap();
        let tags = String::from_utf8_lossy(&tagged.stdout);
        assert!(tags.contains("places/inChina/Beijing"), "tags were not written: {tags}");

        let flat = Command::new("exiftool")
            .args([
                "-s3",
                "-XMP-microsoft:LastKeywordXMP",
                "-XMP-dc:Subject",
                "-IPTC:Keywords",
                &root.join(PHOTOS[1].path).to_string_lossy(),
            ])
            .output()
            .unwrap();
        let flat = String::from_utf8_lossy(&flat.stdout);
        assert!(flat.contains("places/inChina/Beijing"), "hierarchy missing: {flat}");
        assert!(flat.contains("Beijing"), "leaf missing: {flat}");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
