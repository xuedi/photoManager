//! Time zones and XMP dates: every photo with a date gets the offset of where it was taken, or of
//! the zone given, and with it XMP and IPTC dates that agree with EXIF, since the date field
//! writes all of them from the one value. The misleading XMP dates old Shotwell left behind go
//! with it.
//!
//! Where it was taken, a photo that states an offset already keeps it, and one without a position
//! in a country of several zones is refused: its zone is given by hand.

use super::offsets::Offsets;
use crate::cache::{self, Cache, Dated};
use crate::changeset::Wanted;
use crate::dates;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::write::{Change, Field, Taken};

/// Each dated photo of the scope with its offset: in `zone` for every one of them, or without
/// one, where each was taken for those that state none yet.
pub fn wanted(cache: &Cache, geo: Option<&Geo>, scope: &Scope, zone: Option<&str>) -> cache::Result<Vec<Wanted>> {
    let mut offsets = Offsets::new(geo);
    let mut wanted = Vec::new();
    for photo in dated(cache, scope)? {
        if zone.is_none() && photo.taken_offset.is_some() {
            continue;
        }
        let Some(at) = photo.taken_at.clone() else { continue };
        let offset = match zone {
            Some(zone) => dates::offset_in(zone, &at),
            None => offsets.offset(&photo, &at, None),
        };
        wanted.push(match offset {
            Ok(offset) => Wanted::new(
                photo.rel_path,
                Change::of([Field::Taken(Some(Taken {
                    at,
                    offset: Some(offset),
                }))]),
            ),
            Err(why) => Wanted::refused(photo.rel_path, why),
        });
    }
    Ok(wanted)
}

/// The photos of the scope with a date.
fn dated(cache: &Cache, scope: &Scope) -> cache::Result<Vec<Dated>> {
    let paths = scope.paths(cache)?;
    Ok(cache
        .dated(&paths)?
        .into_iter()
        .filter(|photo| photo.taken_at.is_some())
        .collect())
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::ChangeSet;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::offsets::NO_PLACE_DATA;
    use crate::tools::testing::{Library, geo};

    const SEASONS: &str = "Germany/2015-00-00 Seasons";
    const WINTER: &str = "Germany/2015-00-00 Seasons/IMG_8001.JPG";
    const SUMMER: &str = "Germany/2015-00-00 Seasons/IMG_8002.JPG";
    const STATED: &str = "Germany/2015-00-00 Seasons/IMG_8003.JPG";
    const LOOSE: &str = "China/IMG_3140.JPG";

    fn built(library: &Library, geo: Option<&Geo>, scope: &Scope, zone: Option<&str>) -> ChangeSet {
        ChangeSet::build(&library.cache, "", &wanted(&library.cache, geo, scope, zone).unwrap()).unwrap()
    }

    fn within(folder: &str) -> Scope {
        Scope::Filter(Filter::all().within(folder))
    }

    fn offsets(set: &crate::changeset::ChangeSet) -> Vec<(&str, String)> {
        set.rows
            .iter()
            .map(|row| {
                let told = match &row.change.fields.first() {
                    Some(Field::Taken(Some(taken))) => {
                        format!("{} {}", taken.at, taken.offset.clone().unwrap_or_default())
                    }
                    _ => row.verdict.tells(),
                };
                (row.rel_path.as_str(), told)
            })
            .collect()
    }

    #[test]
    fn winter_and_summer_get_their_own_offset_and_a_stated_one_is_kept() {
        let library = Library::new("zones-seasons");
        let set = built(&library, Some(&geo()), &within(SEASONS), None);
        assert_eq!(
            offsets(&set),
            [
                (WINTER, "2015-01-20 11:00:00 +01:00".to_string()),
                (SUMMER, "2015-07-20 11:00:00 +02:00".to_string()),
            ],
            "the photo that states its offset is not in the set"
        );
        assert!(set.rows.iter().all(|row| row.verdict == Verdict::Change));

        let whole = built(&library, Some(&geo()), &Scope::Filter(Filter::all()), None);
        let loose = whole
            .rows
            .iter()
            .find(|row| row.rel_path == LOOSE)
            .expect("the loose photo");
        assert!(
            matches!(&loose.change.fields[0], Field::Taken(Some(taken)) if taken.offset.as_deref() == Some("+08:00")),
            "a photo without a position takes its folder country's zone"
        );
        assert!(whole.rows.iter().all(|row| row.rel_path != STATED));
    }

    #[test]
    fn without_place_data_every_photo_is_refused_and_says_why() {
        let library = Library::new("zones-no-places");
        let set = built(&library, None, &within(SEASONS), None);
        assert_eq!(set.rows.len(), 2);
        assert!(
            set.rows
                .iter()
                .all(|row| row.verdict == Verdict::Refused(NO_PLACE_DATA.to_string()))
        );
        assert_eq!(set.counts().change, 0);
    }

    #[test]
    fn the_xmp_date_is_written_equal_to_exif_and_taken_back_exactly() {
        let mut library = Library::new("zones-write");
        let before = library.dates(SUMMER);
        assert_eq!(before["XMP-xmp:CreateDate"], "2015:07:20 09:00:00Z", "{before:?}");
        assert!(before.contains_key("XMP-exif:DateTimeOriginal"), "{before:?}");
        assert!(before.contains_key("XMP-exif:DateTimeDigitized"), "{before:?}");

        let set = built(&library, Some(&geo()), &within(SEASONS), None);
        let summary = library.apply(&set);
        assert_eq!(summary.written, 2, "{summary:?}");

        let after = library.dates(SUMMER);
        assert_eq!(after["ExifIFD:DateTimeOriginal"], "2015:07:20 11:00:00");
        assert_eq!(after["ExifIFD:OffsetTimeOriginal"], "+02:00");
        assert_eq!(after["XMP-xmp:CreateDate"], "2015:07:20 11:00:00+02:00");
        assert_eq!(after["XMP-photoshop:DateCreated"], "2015:07:20 11:00:00+02:00");
        assert!(
            !after.contains_key("XMP-exif:DateTimeOriginal"),
            "the old Shotwell field is gone: {after:?}"
        );
        assert!(
            !after.contains_key("XMP-exif:DateTimeDigitized"),
            "and its twin: {after:?}"
        );
        let winter = library.dates(WINTER);
        assert_eq!(winter["ExifIFD:OffsetTimeOriginal"], "+01:00");
        assert_eq!(winter["XMP-xmp:CreateDate"], "2015:01:20 11:00:00+01:00");

        library.rescan();
        let again = built(&library, Some(&geo()), &within(SEASONS), None);
        assert!(again.is_empty(), "a second run changes nothing: {:?}", offsets(&again));

        let given = built(&library, None, &within(SEASONS), Some("Asia/Shanghai"));
        let told = offsets(&given);
        assert_eq!(
            told.len(),
            3,
            "a zone given goes to every dated photo, one that states its own too"
        );
        assert!(told.iter().all(|(_, offset)| offset.ends_with("+08:00")), "{told:?}");

        let undone = library.undo();
        assert_eq!(undone.written, 2, "{undone:?}");
        assert_eq!(library.dates(SUMMER), before, "the old XMP dates are back exactly");
    }
}
