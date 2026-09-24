//! Time zones and XMP dates: every photo with a date and no offset gets the offset of where it was
//! taken, and with it XMP and IPTC dates that agree with EXIF, since the date field writes all of
//! them from the one value. The misleading XMP dates old Shotwell left behind go with it.
//!
//! A photo that states an offset already keeps it. The only question is which zone, where a
//! photo without a position is in a country of several.

use std::collections::BTreeMap;

use super::offsets::{Offsets, Zone, zone_key};
use super::{Answer, Answers, Kind, Offer, Question, Tool, Wording};
use crate::cache::{self, Cache, Dated};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::write::{Change, Field, Taken};

pub struct TimeZones;

/// A country of several zones, its zones, and how many photos wait on it.
type Several = (String, Vec<(String, i64)>, usize);

impl Tool for TimeZones {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "time-zones"
    }

    fn title(&self) -> &'static str {
        "Time Zones and XMP Dates"
    }

    fn fixes(&self) -> &'static str {
        "Writes the offset of where each photo was taken and makes its XMP dates agree"
    }

    fn named(&self, _answers: &Answers) -> String {
        "Write time zones and XMP dates".to_string()
    }

    fn answers<'a>(&self, answers: &'a mut Answers) -> Option<&'a mut Answers> {
        Some(answers)
    }

    fn asks_with_place_data(&self) -> bool {
        true
    }

    fn waiting(&self, open: usize) -> String {
        match open {
            1 => "1 folder waits for its time zone".to_string(),
            open => format!("{open} folders wait for their time zone"),
        }
    }

    fn wording(&self) -> Wording {
        Wording {
            asked: "Time Zones",
            one: "folder",
            many: "folders",
            confirm: "",
            unasked: "No photo of the scope is in a country with several time zones. Preview shows the \
                      offset each photo would get.",
            ..Wording::default()
        }
    }

    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let photos = without_offset(cache, scope).map_err(|error| error.to_string())?;
        let mut offsets = Offsets::new(geo);
        let mut asked: BTreeMap<String, Several> = BTreeMap::new();
        for photo in &photos {
            if let Zone::Several { country, zones } = offsets.zone(photo) {
                asked.entry(zone_key(photo)).or_insert((country, zones, 0)).2 += 1;
            }
        }
        let mut questions: Vec<Question> = asked
            .into_iter()
            .map(|(key, (country, zones, photos))| {
                let total: i64 = zones.iter().map(|(_, count)| count).sum();
                let offers = zones
                    .iter()
                    .map(|(zone, count)| Offer {
                        confidence: *count as f64 / total.max(1) as f64,
                        ..Offer::of_answer(Answer::Zone(zone.clone()), zone.clone())
                    })
                    .collect();
                Question {
                    kind: Kind::Zone,
                    answer: answers.get(&key).cloned(),
                    note: Some(format!("{country} has several time zones and these photos no position")),
                    ..Question::place(key.clone(), key.rsplit('/').next().unwrap_or(&key), photos, offers)
                }
            })
            .collect();
        questions.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.key.cmp(&other.key)));
        Ok(questions)
    }

    fn wanted(&self, cache: &Cache, geo: Option<&Geo>, scope: &Scope, answers: &Answers) -> cache::Result<Vec<Wanted>> {
        let mut offsets = Offsets::new(geo);
        let mut wanted = Vec::new();
        for photo in without_offset(cache, scope)? {
            let Some(at) = photo.taken_at.clone() else { continue };
            let chosen = match offsets.zone(&photo) {
                Zone::Several { .. } => match answers.get(&zone_key(&photo)) {
                    Some(Answer::Zone(zone)) => Some(zone.clone()),
                    _ => continue,
                },
                _ => None,
            };
            wanted.push(match offsets.offset(&photo, &at, chosen.as_deref()) {
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
}

/// The photos of the scope with a date and no offset.
fn without_offset(cache: &Cache, scope: &Scope) -> cache::Result<Vec<Dated>> {
    let paths = scope.paths(cache)?;
    Ok(cache
        .dated(&paths)?
        .into_iter()
        .filter(|photo| photo.taken_at.is_some() && photo.taken_offset.is_none())
        .collect())
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::offsets::NO_PLACE_DATA;
    use crate::tools::testing::{Library, geo};
    use crate::tools::{self, AnyTool};

    const SEASONS: &str = "Germany/2015-00-00 Seasons";
    const WINTER: &str = "Germany/2015-00-00 Seasons/IMG_8001.JPG";
    const SUMMER: &str = "Germany/2015-00-00 Seasons/IMG_8002.JPG";
    const STATED: &str = "Germany/2015-00-00 Seasons/IMG_8003.JPG";
    const LOOSE: &str = "China/IMG_3140.JPG";

    fn tool() -> &'static dyn AnyTool {
        tools::find("time-zones").expect("the tool is listed")
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
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &within(SEASONS), None)
            .unwrap();
        assert_eq!(set.title, "Write time zones and XMP dates");
        assert_eq!(
            offsets(&set),
            [
                (WINTER, "2015-01-20 11:00:00 +01:00".to_string()),
                (SUMMER, "2015-07-20 11:00:00 +02:00".to_string()),
            ],
            "the photo that states its offset is not in the set"
        );
        assert!(set.rows.iter().all(|row| row.verdict == Verdict::Change));

        let whole = tool()
            .change_set(&library.cache, Some(&geo()), &Scope::Filter(Filter::all()), None)
            .unwrap();
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
        assert_eq!(
            tools::waiting(
                tool(),
                &library.cache,
                Some(&geo()),
                &Scope::Filter(Filter::all()),
                None
            )
            .unwrap(),
            0,
            "no fixture country has several zones"
        );
    }

    #[test]
    fn without_place_data_every_photo_is_refused_and_says_why() {
        let library = Library::new("zones-no-places");
        let set = tool().change_set(&library.cache, None, &within(SEASONS), None).unwrap();
        assert_eq!(set.rows.len(), 2);
        assert!(
            set.rows
                .iter()
                .all(|row| row.verdict == Verdict::Refused(NO_PLACE_DATA.to_string()))
        );
        assert_eq!(
            tools::count(tool(), &library.cache, None, &within(SEASONS), None).unwrap(),
            0
        );
    }

    #[test]
    fn the_xmp_date_is_written_equal_to_exif_and_taken_back_exactly() {
        let mut library = Library::new("zones-write");
        let before = library.dates(SUMMER);
        assert_eq!(before["XMP-xmp:CreateDate"], "2015:07:20 09:00:00Z", "{before:?}");
        assert!(before.contains_key("XMP-exif:DateTimeOriginal"), "{before:?}");
        assert!(before.contains_key("XMP-exif:DateTimeDigitized"), "{before:?}");

        let set = tool()
            .change_set(&library.cache, Some(&geo()), &within(SEASONS), None)
            .unwrap();
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
        let again = tool()
            .change_set(&library.cache, Some(&geo()), &within(SEASONS), None)
            .unwrap();
        assert!(again.is_empty(), "a second run changes nothing: {:?}", offsets(&again));

        let undone = library.undo();
        assert_eq!(undone.written, 2, "{undone:?}");
        assert_eq!(library.dates(SUMMER), before, "the old XMP dates are back exactly");
    }
}
