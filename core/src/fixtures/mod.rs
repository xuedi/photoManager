//! A small stand-in library with the shapes the real one has: events without a full date, a
//! loose file in a country folder, sub-folders, photos without GPS but with a place tag, a
//! photo without any date, XMP dates that disagree with EXIF, and one without tags at all. The
//! tags are as untidy as real ones: a second spelling of a root, case twins, a typo, a city
//! misspelled, a place that is no place, one photo with two places tags, a misspelled root, a bare
//! root, a tag in only one of the tag fields, a keyword left in the label and a catalog set, and an
//! old photo with flat keywords only: one tag of the tree has its name, one a tag spelled two ways,
//! two tags another and none the last. No tagged photo writes every level into every tag field, as
//! none in the real library does. Some events are
//! partly placed: one whose located photos all stand in one city and one photo does not, one
//! whose photos were placed in two cities, and one named `Wedding` in another city's folder. The
//! dates have the real shapes too: a winter and a summer photo of one event, an XMP date two hours
//! off, a photo that states its offset, a second camera years off in one event, a folder a month
//! off, an event without camera names, and photos without a date between dated ones, at the end of
//! their folder and in a month folder. One photo carries the small picture cameras embed, the rest
//! do not. The one turned by its orientation is
//! stored wider than tall, so the right way up it stands taller than wide. One photo carries the
//! face boxes an older program drew, and one names a person without a box. One event mixes a
//! phone that measured where it was, in the event folder and in a sub-folder, with a camera whose
//! photos stand on the centre a places tag gave, or on nothing at all. A photo tagged with a person
//! who has a box in another photo names nobody itself; a located photo carries a places tag finer
//! than its town, and another the tag of a town far from where it stands. One photo names its
//! event in its own field as its folder does, and one names another event than its folder. A
//! photo without a date carries a year tag, which is then the only record of its year.
//!
//! Only for tests and for looking at the application without touching real photos.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;
use std::process::Command;

/// Face boxes an older program drew: one on a face Immich also finds, named as the tag names the
/// person, and one on a face Immich does not know.
const OLDER_REGIONS: &str = "-XMP-mwg-rs:RegionInfo={AppliedToDimensions={W=16,H=16,Unit=pixel},\
    RegionList=[{Area={X=0.26,Y=0.3,W=0.28,H=0.38,Unit=normalized},Name=Anna,Type=Face},\
    {Area={X=0.5,Y=0.85,W=0.1,H=0.1,Unit=normalized},Name=Tom,Type=Face}]}";

/// One distinct image per photo, so every photo has its own content id.
const IMAGES: [&[u8]; 44] = [
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
    include_bytes!("p40.jpg"),
    include_bytes!("p41.jpg"),
    include_bytes!("p42.jpg"),
    include_bytes!("p43.jpg"),
    include_bytes!("p44.jpg"),
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
            "-TagsList=people/family/Anna",
            "-XMP-mediapro:CatalogSets=Holiday",
        ],
    },
    Photo {
        path: "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG",
        metadata: &[
            "-TagsList=timeline/2008",
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
            "-XMP-iptcExt:Event=Wedding Trip",
        ],
    },
    Photo {
        path: "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG",
        metadata: &[
            "-DateTimeOriginal=2018:10:06 14:03:40",
            "-Model=X100S",
            "-XMP-iptcExt:PersonInImage=Mia",
        ],
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
            "-XMP-microsoft:LastKeywordXMP=people/family/Tom",
            OLDER_REGIONS,
            "-XMP-iptcExt:PersonInImage=Anna",
            "-XMP-iptcExt:Event=Sommerfest",
            "-XMP-iptcExt:PersonInImage=Tom",
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
            "-TagsList=mixed/disgusting",
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
        metadata: &[
            "-DateTimeOriginal=2012:04:05 12:00:00",
            "-TagsList=places/inChina",
            "-TagsList=events",
        ],
    },
    Photo {
        path: "Germany/Hamburg/2014-08-00 Wedding/IMG_2001.JPG",
        metadata: &[
            "-DateTimeOriginal=2014:08:16 14:00:00",
            "-TagsList=places/inGermany",
            "-TagsList=Apartmens/Harbour Flat",
        ],
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
            "-TagsList=places/inGermany/Harbourside",
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
            "-TagsList=places/inGermany/Bremen",
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
            "-IPTC:Keywords=inChina",
            "-IPTC:Keywords=Funny",
            "-IPTC:Keywords=food",
            "-IPTC:Keywords=landscape",
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
            "-TagsList=topics/food",
        ],
    },
    Photo {
        path: "Germany/2018-05-12 Canal Tour/PXL_0001.jpg",
        metadata: &[
            "-DateTimeOriginal=2018:05:12 14:02:00",
            "-Make=Google",
            "-Model=Pixel 3",
            "-GPSLatitude=53.5485",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9780",
            "-GPSLongitudeRef=E",
        ],
    },
    Photo {
        path: "Germany/2018-05-12 Canal Tour/DSCF0201.JPG",
        metadata: &[
            "-DateTimeOriginal=2018:05:12 14:03:10",
            "-Make=FUJIFILM",
            "-Model=X100S",
            "-GPSLatitude=53.5511",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9937",
            "-GPSLongitudeRef=E",
            "-GPSProcessingMethod=photoManager: places tag",
            "-GPSHPositioningError=5000",
            "-TagsList=places/inGermany/Hamburg",
        ],
    },
    Photo {
        path: "Germany/2018-05-12 Canal Tour/DSCF0202.JPG",
        metadata: &[
            "-DateTimeOriginal=2018:05:12 14:40:00",
            "-Make=FUJIFILM",
            "-Model=X100S",
            "-GPSLatitude=53.5511",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9937",
            "-GPSLongitudeRef=E",
            "-GPSProcessingMethod=photoManager: places tag",
            "-GPSHPositioningError=5000",
            "-TagsList=places/inGermany/Hamburg",
        ],
    },
    Photo {
        path: "Germany/2018-05-12 Canal Tour/DSCF0203.JPG",
        metadata: &["-Make=FUJIFILM", "-Model=X100S"],
    },
    Photo {
        path: "Germany/2018-05-12 Canal Tour/Evening/PXL_0002.jpg",
        metadata: &[
            "-DateTimeOriginal=2018:05:12 19:00:00",
            "-Make=Google",
            "-Model=Pixel 3",
            "-GPSLatitude=53.5430",
            "-GPSLatitudeRef=N",
            "-GPSLongitude=9.9690",
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
/// A photo whose camera wrote a maker note ExifTool doubts: an Olympus-style maker note whose
/// focus directory announces more entries than it holds. ExifTool reads it, but rewrites the file
/// only when told to ignore that minor problem, as it does with some real cameras. The maker note
/// goes into `jpeg`, which must not carry EXIF yet; without one, into one of the invented
/// pictures. Every byte of it is made up here.
pub fn with_doubted_maker_note(jpeg: Option<&[u8]>) -> Vec<u8> {
    fn directory(entries: &[(u16, u16, u32, [u8; 4])]) -> Vec<u8> {
        let mut bytes = (entries.len() as u16).to_le_bytes().to_vec();
        for (tag, kind, count, value) in entries {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(kind.to_le_bytes());
            bytes.extend(count.to_le_bytes());
            bytes.extend(value);
        }
        bytes.extend(0u32.to_le_bytes());
        bytes
    }
    let make = b"OLYMPUS\0";
    let make_at = 8 + 2 + 3 * 12 + 4;
    let exif_at = make_at + make.len() as u32;
    let note_at = exif_at + 2 + 12 + 4;
    let values_at = note_at + 8 + 2 + 2 * 12 + 4;
    let special: Vec<u8> = [0u32, 0, 100].iter().flat_map(|value| value.to_le_bytes()).collect();
    let focus_at = values_at + special.len() as u32;
    let mut focus = 40u16.to_le_bytes().to_vec();
    focus.extend(directory(&[(0x0209, 3, 1, 7u32.to_le_bytes())]));
    focus.extend([0u8; 4]);
    let mut note = b"OLYMP\0\x01\0".to_vec();
    note.extend(directory(&[
        (0x0200, 4, 3, values_at.to_le_bytes()),
        (0x2050, 7, focus.len() as u32, focus_at.to_le_bytes()),
    ]));
    note.extend(special);
    note.extend(focus);
    let mut tiff = b"II*\0".to_vec();
    tiff.extend(8u32.to_le_bytes());
    tiff.extend(directory(&[
        (0x010f, 2, make.len() as u32, make_at.to_le_bytes()),
        (0x0110, 2, 4, *b"X1\0\0"),
        (0x8769, 4, 1, exif_at.to_le_bytes()),
    ]));
    tiff.extend(make);
    tiff.extend(directory(&[(0x927c, 7, note.len() as u32, note_at.to_le_bytes())]));
    tiff.extend(note);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let image = jpeg.unwrap_or(IMAGES[0]);
    let mut jpeg = image[..2].to_vec();
    jpeg.extend([0xff, 0xe1]);
    jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend(app1);
    jpeg.extend(&image[2..]);
    jpeg
}

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
