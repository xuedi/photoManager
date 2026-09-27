//! The name a photo should have: when it was taken, `2019-07-14_153012.jpg`, and `_2`, `_3` for
//! more photos of the same second in one folder. A name that already fits the photo's date is
//! settled and never numbered again, and no photo is ever given a name something in its folder
//! already has, so a rename can neither overwrite a file nor wait on another one.

use std::collections::HashSet;

use crate::dates;

pub const EXTENSION: &str = "jpg";

/// One photo of a folder, by its file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub name: String,
    /// `YYYY-MM-DD HH:MM:SS`, the camera's own clock.
    pub taken_at: Option<String>,
    /// The digits of `SubSecTimeOriginal`, a fraction of the second.
    pub sub_second: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Settled,
    Rename(String),
    Refused(String),
}

/// What becomes of every photo of one folder, in the order given. `on_disk` is every name in the
/// folder, photo or not; none of them is handed out, told apart without regard to case, because a
/// synced copy on another computer may not tell `a.jpg` from `A.jpg`.
pub fn plan(photos: &[Photo], on_disk: &[String]) -> Vec<Verdict> {
    let mut taken: HashSet<String> = on_disk.iter().map(|name| name.to_lowercase()).collect();
    let mut verdicts: Vec<Option<Verdict>> = vec![None; photos.len()];
    let mut moving = Vec::new();
    for (index, photo) in photos.iter().enumerate() {
        match photo.taken_at.as_deref().and_then(stem) {
            None => verdicts[index] = Some(Verdict::Refused("it has no date".to_string())),
            Some(stem) if fits(&photo.name, &stem) => verdicts[index] = Some(Verdict::Settled),
            Some(stem) => moving.push((index, stem)),
        }
    }
    moving.sort_by(|(a, a_stem), (b, b_stem)| {
        let order = |index: usize| {
            (
                fraction(photos[index].sub_second.as_deref()),
                photos[index].name.as_str(),
            )
        };
        a_stem.cmp(b_stem).then_with(|| order(*a).cmp(&order(*b)))
    });
    for (index, stem) in moving {
        let own = photos[index].name.to_lowercase();
        let name = (1..)
            .map(|number| numbered(&stem, number))
            .find(|name| {
                let lower = name.to_lowercase();
                lower == own || !taken.contains(&lower)
            })
            .expect("a free number");
        taken.insert(name.to_lowercase());
        verdicts[index] = Some(Verdict::Rename(name));
    }
    verdicts
        .into_iter()
        .map(|verdict| verdict.expect("every photo judged"))
        .collect()
}

/// `2019-07-14_153012`, if the date is a real one.
fn stem(taken_at: &str) -> Option<String> {
    let at = dates::parse(taken_at).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}_{:02}{:02}{:02}",
        at.year(),
        at.month(),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    ))
}

fn numbered(stem: &str, number: u32) -> String {
    match number {
        1 => format!("{stem}.{EXTENSION}"),
        _ => format!("{stem}_{number}.{EXTENSION}"),
    }
}

/// Whether a name is one of those the scheme gives this date: the bare one or a number from 2 on.
fn fits(name: &str, stem: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_suffix(&format!(".{EXTENSION}")))
    else {
        return false;
    };
    match rest.strip_prefix('_') {
        None => rest.is_empty(),
        Some(number) => !number.starts_with('0') && number.parse::<u32>().is_ok_and(|number| number >= 2),
    }
}

/// The digits of a fraction of a second, so `5` comes after `45`; none comes first.
fn fraction(sub_second: Option<&str>) -> String {
    let digits: String = sub_second
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    format!("{digits:0<9}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(name: &str, taken_at: Option<&str>, sub_second: Option<&str>) -> Photo {
        Photo {
            name: name.to_string(),
            taken_at: taken_at.map(str::to_string),
            sub_second: sub_second.map(str::to_string),
        }
    }

    fn names(photos: &[Photo]) -> Vec<String> {
        photos.iter().map(|photo| photo.name.clone()).collect()
    }

    fn renamed(name: &str) -> Verdict {
        Verdict::Rename(name.to_string())
    }

    #[test]
    fn camera_names_become_the_date_they_were_taken() {
        let photos = [
            photo("IMG_4711.JPG", Some("2019-07-14 15:30:12"), None),
            photo("DSCF0002.jpg", Some("2019-07-14 09:05:00"), None),
            photo("p1000003.jpeg", Some("2019-07-15 23:59:59"), None),
        ];
        assert_eq!(
            plan(&photos, &names(&photos)),
            [
                renamed("2019-07-14_153012.jpg"),
                renamed("2019-07-14_090500.jpg"),
                renamed("2019-07-15_235959.jpg")
            ]
        );
    }

    #[test]
    fn photos_of_one_second_are_numbered_by_the_fraction_then_the_old_name() {
        let photos = [
            photo("IMG_0003.JPG", Some("2019-07-14 15:30:12"), Some("5")),
            photo("IMG_0002.JPG", Some("2019-07-14 15:30:12"), Some("45")),
            photo("IMG_0009.JPG", Some("2019-07-14 15:30:12"), None),
            photo("IMG_0001.JPG", Some("2019-07-14 15:30:12"), None),
        ];
        assert_eq!(
            plan(&photos, &names(&photos)),
            [
                renamed("2019-07-14_153012_4.jpg"),
                renamed("2019-07-14_153012_3.jpg"),
                renamed("2019-07-14_153012_2.jpg"),
                renamed("2019-07-14_153012.jpg")
            ]
        );
    }

    #[test]
    fn a_name_that_fits_its_date_is_settled() {
        let photos = [
            photo("2019-07-14_153012.jpg", Some("2019-07-14 15:30:12"), None),
            photo("2019-07-14_153012_3.jpg", Some("2019-07-14 15:30:12"), None),
        ];
        assert_eq!(plan(&photos, &names(&photos)), [Verdict::Settled, Verdict::Settled]);
    }

    #[test]
    fn a_name_that_only_looks_like_the_scheme_is_not_settled() {
        for name in [
            "2019-07-14_153012.JPG",
            "2019-07-14_153012_1.jpg",
            "2019-07-14_153012_02.jpg",
            "2019-07-14_153013.jpg",
            "2019-07-14_153012_x.jpg",
        ] {
            let photos = [photo(name, Some("2019-07-14 15:30:12"), None)];
            assert_eq!(
                plan(&photos, &names(&photos)),
                [renamed("2019-07-14_153012.jpg")],
                "{name}"
            );
        }
    }

    #[test]
    fn a_new_photo_takes_the_next_free_number_and_nobody_else_moves() {
        let photos = [
            photo("2019-07-14_153012.jpg", Some("2019-07-14 15:30:12"), None),
            photo("2019-07-14_153012_2.jpg", Some("2019-07-14 15:30:12"), None),
            photo("IMG_0001.JPG", Some("2019-07-14 15:30:12"), Some("0")),
        ];
        assert_eq!(
            plan(&photos, &names(&photos)),
            [Verdict::Settled, Verdict::Settled, renamed("2019-07-14_153012_3.jpg")]
        );
    }

    #[test]
    fn a_photo_without_a_date_keeps_its_name() {
        let photos = [
            photo("IMG_0001.JPG", None, None),
            photo("IMG_0002.JPG", Some("2019-07-00 00:00:00"), None),
            photo("IMG_0003.JPG", Some("2019-07-14"), None),
        ];
        let refused = Verdict::Refused("it has no date".to_string());
        assert_eq!(
            plan(&photos, &names(&photos)),
            [refused.clone(), refused.clone(), refused]
        );
    }

    #[test]
    fn a_name_anything_in_the_folder_has_is_never_given_whatever_its_case() {
        let photos = [photo("IMG_0001.JPG", Some("2019-07-14 15:30:12"), None)];
        let on_disk = [
            "IMG_0001.JPG".to_string(),
            "2019-07-14_153012.JPG.xmp".to_string(),
            "2019-07-14_153012.JPG".to_string(),
            "notes.txt".to_string(),
        ];
        assert_eq!(plan(&photos, &on_disk), [renamed("2019-07-14_153012_2.jpg")]);
    }

    #[test]
    fn only_the_case_of_its_own_extension_changes() {
        let photos = [photo("2019-07-14_153012.JPG", Some("2019-07-14 15:30:12"), None)];
        assert_eq!(plan(&photos, &names(&photos)), [renamed("2019-07-14_153012.jpg")]);
    }

    #[test]
    fn two_photos_never_swap_or_chain_their_names() {
        let photos = [
            photo("2019-07-14_153012.jpg", Some("2020-01-01 10:00:00"), None),
            photo("2020-01-01_100000.jpg", Some("2019-07-14 15:30:12"), None),
        ];
        assert_eq!(
            plan(&photos, &names(&photos)),
            [renamed("2020-01-01_100000_2.jpg"), renamed("2019-07-14_153012_2.jpg")]
        );
    }
}
