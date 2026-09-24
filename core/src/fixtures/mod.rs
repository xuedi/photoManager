//! A small stand-in library with the shapes the real one has: events without a full date, a
//! loose file in a country folder, sub-folders, photos without GPS but with a place tag, a
//! photo without any date, XMP dates that disagree with EXIF, and one without tags at all. The
//! tags are as untidy as real ones: a second spelling of a root, case twins, a typo, a city
//! misspelled, a place that is no place, and one photo with two places tags. Some events are
//! partly placed: one whose located photos all stand in one city and one photo does not, one
//! whose photos were placed in two cities, and one named `Wedding` in another city's folder. The
//! dates have the real shapes too: a winter and a summer photo of one event, an XMP date two hours
//! off, a photo that states its offset, a second camera years off in one event, a folder a month
//! off, an event without camera names, and photos without a date between dated ones, at the end of
//! their folder and in a month folder. One photo carries the small picture cameras embed, the rest
//! do not. The one turned by its orientation is
//! stored wider than tall, so the right way up it stands taller than wide.
//!
//! Only for tests and for looking at the application without touching real photos.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;
use std::process::Command;

/// One distinct image per photo, so every photo has its own content id.
const IMAGES: [&[u8]; 39] = [
    include_bytes!("p01.jpg"),
    include_bytes!("p02.jpg"),
    include_bytes!("p03.jpg"),
    include_bytes!("p04.jpg"),
    include_bytes!("p05.jpg"),
    include_bytes!("p06.jpg"),
    include_bytes!("p07.jpg"),
    include_bytes!("p08.jpg"),
    include_bytes!("p09.jpg"),
    include_bytes!("p10.jpg"),
    include_bytes!("p11.jpg"),
    include_bytes!("p12.jpg"),
    include_bytes!("p13.jpg"),
    include_bytes!("p14.jpg"),
    include_bytes!("p15.jpg"),
    include_bytes!("p16.jpg"),
    include_bytes!("p17.jpg"),
    include_bytes!("p18.jpg"),
    include_bytes!("p19.jpg"),
    include_bytes!("p20.jpg"),
    include_bytes!("p21.jpg"),
    include_bytes!("p22.jpg"),
    include_bytes!("p23.jpg"),
    include_bytes!("p24.jpg"),
    include_bytes!("p25.jpg"),
    include_bytes!("p26.jpg"),
    include_bytes!("p27.jpg"),
    include_bytes!("p28.jpg"),
    include_bytes!("p29.jpg"),
    include_bytes!("p30.jpg"),
    include_bytes!("p31.jpg"),
    include_bytes!("p32.jpg"),
    include_bytes!("p33.jpg"),
    include_bytes!("p34.jpg"),
    include_bytes!("p35.jpg"),
    include_bytes!("p36.jpg"),
    include_bytes!("p37.jpg"),
    include_bytes!("p38.jpg"),
    include_bytes!("p39.jpg"),
];

struct Photo {
    path: &'static str,
    metadata: &'static [&'static str],
}

const PLACES_CHINA: &str = "-TagsList=places/inChina/Beijing";
const PLACES_DENMARK: &str = "-TagsList=places/inDenmark/Copenhagen";

const PHOTOS: &[Photo] = &[
    Photo {
        path: "China/IMG_3140.JPG",
        metadata: &["-DateTimeOriginal=2006:09:14 10:12:00", "-Model=Canon PowerShot A640"],
    },
    Photo {
        path: "China/2006-09-00 Besuch Ben/P1000001.JPG",
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
        metadata: &[
            "-DateTimeOriginal=2006:08:21 16:45:00",
            "-Model=Panasonic DMC-LS1",
            PLACES_CHINA,
            "-TagsList=people/groupChina/Ben",
        ],
    },
    Photo {
        path: "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG",
        metadata: &[
            "-TagsList=places/inChina",
            "-TagsList=mixed/food",
            "-TagsList=mixed/funny",
        ],
    },
    Photo {
        path: "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG",
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
        metadata: &["-DateTimeOriginal=2018:10:06 14:03:40", "-Model=X100S"],
    },
    Photo {
        path: "Germany/2019-07-13 Sommerfest/img_0657.jpg",
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
        metadata: &[
            "-DateTimeOriginal=2019:07:13 19:05:00",
            "-XMP-xmp:CreateDate=2019:07:13 17:05:00Z",
            "-XMP-xmp:Label=timeline",
            "-TagsList=places/inGermany/Hamburg",
        ],
    },
    Photo {
        path: "Germany/2019-07-13 Sommerfest/IMAG0001.jpg",
        metadata: &[
            "-DateTimeOriginal=2019:07:13 20:41:00",
            "-TagsList=people/family/Anna",
            "-TagsList=people/me",
            "-TagsList=places/inGermany",
        ],
    },
    Photo {
        path: "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG",
        metadata: &[
            "-DateTimeOriginal=2008:10:03 11:15:00",
            "-TagsList=places/inIreland/Galway",
            "-TagsList=People/Kira",
            "-TagsList=mixed/disgusting",
        ],
    },
    Photo {
        path: "Ireland/2008-10-03 Galway/IMG_0003.JPG",
        metadata: &[
            "-DateTimeOriginal=2008:10:03 12:47:00",
            "-TagsList=places/inNetherland/Amsterdam",
            "-TagsList=mixed/Funny",
            "-XMP-photoshop:City=Galway",
            "-IPTC:City=Galway",
        ],
    },
    Photo {
        path: "Greece/0000-00-00 Aeron ilands/IMG_0004.JPG",
        metadata: &["-TagsList=places/inGreece/athens", "-TagsList=mixed/discusting"],
    },
    Photo {
        path: "Greece/0000-00-00 Aeron ilands/IMG_0005.JPG",
        metadata: &[
            "-DateTimeOriginal=2011:05:02 10:04:00",
            "-TagsList=places/inGreece/Atens",
        ],
    },
    Photo {
        path: "Greece/0000-00-00 Aeron ilands/IMG_0006.JPG",
        metadata: &[
            "-DateTimeOriginal=2011:05:03 17:30:00",
            "-TagsList=places/inGreece/AthensSeaSide",
        ],
    },
    Photo {
        path: "Greece/0000-00-00 Aeron ilands/IMG_0007.JPG",
        metadata: &[
            "-DateTimeOriginal=2011:05:04 09:12:00",
            "-TagsList=places/inGreece/athens",
            "-TagsList=places/inGreece/Atens",
        ],
    },
    Photo {
        path: "Germany/2016-06-00 Harbour Walk/DSC_0101.JPG",
        metadata: &[
            "-DateTimeOriginal=2016:06:11 10:02:00",
            "-GPSLatitude=53.5445",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9660",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2016-06-00 Harbour Walk/DSC_0102.JPG",
        metadata: &[
            "-DateTimeOriginal=2016:06:11 10:40:00",
            "-GPSLatitude=53.5412",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9840",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2016-06-00 Harbour Walk/DSC_0103.JPG",
        metadata: &[
            "-DateTimeOriginal=2016:06:11 11:15:00",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2016-06-00 Harbour Walk/DSC_0104.JPG",
        metadata: &["-DateTimeOriginal=2016:06:11 12:30:00"],
    },
    Photo {
        path: "China/2012-04-00 Rail Trip/IMG_5001.JPG",
        metadata: &[
            "-DateTimeOriginal=2012:04:03 09:00:00",
            "-GPSLatitude=39.9075",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=116.39723",
            "-GPSLongitudeRef=E",
            "-GPSProcessingMethod=photoManager: places tag",
            "-GPSHPositioningError=5000",
            PLACES_CHINA,
        ],
    },
    Photo {
        path: "China/2012-04-00 Rail Trip/IMG_5002.JPG",
        metadata: &[
            "-DateTimeOriginal=2012:04:06 15:20:00",
            "-GPSLatitude=38.91222",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=121.60222",
            "-GPSLongitudeRef=E",
            "-GPSProcessingMethod=photoManager: places tag",
            "-GPSHPositioningError=5000",
            "-TagsList=places/inChina/Dalian",
        ],
    },
    Photo {
        path: "China/2012-04-00 Rail Trip/IMG_5003.JPG",
        metadata: &["-DateTimeOriginal=2012:04:05 12:00:00", "-TagsList=places/inChina"],
    },
    Photo {
        path: "Germany/Hamburg/2014-08-00 Wedding/IMG_2001.JPG",
        metadata: &["-DateTimeOriginal=2014:08:16 14:00:00", "-TagsList=places/inGermany"],
    },
    Photo {
        path: "Germany/2015-00-00 Seasons/IMG_8001.JPG",
        metadata: &[
            "-DateTimeOriginal=2015:01:20 11:00:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2015-00-00 Seasons/IMG_8002.JPG",
        metadata: &[
            "-DateTimeOriginal=2015:07:20 11:00:00",
            "-Model=Canon EOS 5D",
            "-XMP-xmp:CreateDate=2015:07:20 09:00:00Z",
            "-XMP-exif:DateTimeOriginal=2015:07:20 09:00:00Z",
            "-XMP-exif:DateTimeDigitized=2015:07:20 09:00:00Z",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2015-00-00 Seasons/IMG_8003.JPG",
        metadata: &[
            "-DateTimeOriginal=2015:07:21 12:00:00",
            "-OffsetTimeOriginal=+02:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2013-05-18 Garden Party/IMG_6001.JPG",
        metadata: &[
            "-DateTimeOriginal=2013:05:18 14:00:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2013-05-18 Garden Party/IMG_6002.JPG",
        metadata: &[
            "-DateTimeOriginal=2013:05:18 15:00:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2013-05-18 Garden Party/IMG_6003.JPG",
        metadata: &[
            "-DateTimeOriginal=2013:05:18 16:00:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2013-05-18 Garden Party/P1060001.JPG",
        metadata: &[
            "-DateTimeOriginal=2011:09:02 15:00:00",
            "-Model=DMC-TZ7",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2013-05-18 Garden Party/P1060002.JPG",
        metadata: &[
            "-DateTimeOriginal=2011:09:02 15:30:00",
            "-Model=DMC-TZ7",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Denmark/2017-09-00 Autumn Walk/DSCF0101.JPG",
        metadata: &[
            "-DateTimeOriginal=2017:08:26 10:00:00",
            "-Model=X100S",
            "-GPSLatitude=55.6761",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=12.5683",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Denmark/2017-09-00 Autumn Walk/DSCF0102.JPG",
        metadata: &[
            "-DateTimeOriginal=2017:08:26 11:30:00",
            "-Model=X100S",
            "-GPSLatitude=55.6761",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=12.5683",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Greece/2010-04-10 Beach/scan0001.jpg",
        metadata: &[
            "-DateTimeOriginal=2009:04:10 10:00:00",
            "-GPSLatitude=37.9838",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=23.7275",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Greece/2010-04-10 Beach/scan0002.jpg",
        metadata: &[
            "-DateTimeOriginal=2009:04:10 11:00:00",
            "-GPSLatitude=37.9838",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=23.7275",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2014-03-22 Museum/IMG_9001.JPG",
        metadata: &[
            "-DateTimeOriginal=2014:03:22 10:00:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2014-03-22 Museum/IMG_9002.JPG",
        metadata: &[
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2014-03-22 Museum/IMG_9003.JPG",
        metadata: &[
            "-DateTimeOriginal=2014:03:22 10:20:00",
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2014-03-22 Museum/IMG_9004.JPG",
        metadata: &[
            "-Model=Canon EOS 5D",
            "-GPSLatitude=53.5500",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9930",
            "-GPSLongitudeRef=E",
        ],
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

/// The photo that carries an embedded EXIF thumbnail, made from its own image.
pub const WITH_EXIF_THUMBNAIL: &str = "Greece/0000-00-00 Aeron ilands/IMG_0004.JPG";

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

    for (index, photo) in PHOTOS.iter().enumerate() {
        let file = root.join(photo.path);
        std::fs::create_dir_all(file.parent().expect("a parent directory"))?;
        std::fs::write(&file, IMAGES[index % IMAGES.len()])?;

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
        if photo.path == WITH_EXIF_THUMBNAIL {
            embed_thumbnail(&file, IMAGES[index % IMAGES.len()])?;
        }
    }
    Ok(())
}

/// ExifTool reads the thumbnail from a file, so the image goes through one outside the library.
fn embed_thumbnail(file: &Path, image: &[u8]) -> Result<()> {
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let thumbnail = std::env::temp_dir().join(format!(
        "photomanager-fixture-thumbnail-{}-{made}.jpg",
        std::process::id()
    ));
    std::fs::write(&thumbnail, image)?;
    let status = Command::new("exiftool")
        .arg("-overwrite_original")
        .arg("-q")
        .arg(format!("-ThumbnailImage<={}", thumbnail.display()))
        .arg(file)
        .status();
    let _ = std::fs::remove_file(&thumbnail);
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err(Error::other(format!(
            "exiftool could not embed a thumbnail in {}",
            file.display()
        ))),
    }
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
