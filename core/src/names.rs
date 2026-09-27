//! The name a photo should have: when it was taken, `2019-07-14_153012.jpg`, and `_2`, `_3` for
//! more photos of the same second in one folder. A name that already fits the photo's date is
//! settled and never numbered again, and no photo is ever given a name something in its folder
//! already has, so a rename can neither overwrite a file nor wait on another one.

use std::collections::{BTreeMap, HashSet};

use crate::cache::Cache;
use crate::changeset::Wanted;
use crate::dates;
use crate::tools::folders::PEOPLE_FIRST;
use crate::tools::people;
use crate::write::Move;

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

/// One folder whose photos are not all named by their date: a move of its own for every photo
/// that gets a new name, and a refused row for every one that keeps its name for want of a date.
#[derive(Debug, Clone)]
pub struct Folder {
    /// Relative to the library, empty for the library itself.
    pub dir: String,
    pub wanted: Vec<Wanted>,
    pub renamed: usize,
    pub undated: usize,
    /// Why some of its photos wait for their people, when they do.
    pub waits: Option<String>,
}

/// Every folder of the library with a photo to rename, in path order.
pub fn folders(cache: &Cache) -> Result<Vec<Folder>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let mut on_disk: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel_path in cache.files().map_err(failed)? {
        let (dir, name) = split(&rel_path);
        on_disk.entry(dir.to_string()).or_default().push(name.to_string());
    }
    let mut by_dir: BTreeMap<String, Vec<(Photo, String, Option<String>)>> = BTreeMap::new();
    for named in cache.named().map_err(failed)? {
        let (dir, name) = split(&named.rel_path);
        let photo = Photo {
            name: name.to_string(),
            taken_at: named.taken_at,
            sub_second: named.sub_second,
        };
        by_dir
            .entry(dir.to_string())
            .or_default()
            .push((photo, named.rel_path.clone(), named.content_id));
    }

    let mut found = Vec::new();
    for (dir, photos) in by_dir {
        let judged: Vec<Photo> = photos.iter().map(|(photo, _, _)| photo.clone()).collect();
        let verdicts = plan(&judged, on_disk.get(&dir).map(Vec::as_slice).unwrap_or_default());
        let mut folder = Folder {
            dir: dir.clone(),
            wanted: Vec::new(),
            renamed: 0,
            undated: 0,
            waits: None,
        };
        for ((_, rel_path, content_id), verdict) in photos.into_iter().zip(verdicts) {
            match verdict {
                Verdict::Settled => {}
                Verdict::Refused(why) => {
                    folder.undated += 1;
                    folder.wanted.push(Wanted::refused(rel_path, why));
                }
                Verdict::Rename(name) => {
                    folder.renamed += 1;
                    let refused = content_id
                        .is_none()
                        .then(|| format!("the scan could not read the image data of {rel_path}"));
                    let moved = Move {
                        photos: content_id.map(|id| vec![(rel_path.clone(), id)]).unwrap_or_default(),
                        from: rel_path,
                        to: joined(&dir, &name),
                    };
                    folder.wanted.push(Wanted::moving(moved, refused));
                }
            }
        }
        if folder.renamed > 0 {
            found.push(folder);
        }
    }

    let moving: Vec<String> = found
        .iter()
        .flat_map(|folder| folder.wanted.iter().filter(|one| one.moved.is_some()))
        .map(|one| one.rel_path.clone())
        .collect();
    if let Some(untold) = people::untold(cache, &moving)? {
        for folder in &mut found {
            for one in folder.wanted.iter_mut().filter(|one| one.moved.is_some()) {
                let Some(names) = untold.get(&one.rel_path).filter(|names| !names.is_empty()) else {
                    continue;
                };
                let why = format!(
                    "{PEOPLE_FIRST} {} in it and the file does not: write the people first",
                    names.join(", ")
                );
                folder.waits.get_or_insert_with(|| why.clone());
                one.refused.get_or_insert(why);
            }
        }
    }
    Ok(found)
}

fn split(rel_path: &str) -> (&str, &str) {
    rel_path.rsplit_once('/').unwrap_or(("", rel_path))
}

fn joined(dir: &str, name: &str) -> String {
    match dir.is_empty() {
        true => name.to_string(),
        false => format!("{dir}/{name}"),
    }
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

#[cfg(all(test, feature = "fixtures"))]
mod library_tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use crate::changeset::Verdict as Row;
    use crate::fixes::{self, Fix};
    use crate::immich::{self, fake};
    use crate::tools::testing::{Library, geo};

    const WEDDING: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    const HARBOUR: &str = "Germany/2016-06-00 Harbour Walk";
    const BEN: &str = "China/2006-09-00 Besuch Ben";

    fn exiftool(root: &Path, rel_path: &str, args: &[&str]) {
        let done = std::process::Command::new("exiftool")
            .args(args)
            .arg("-overwrite_original")
            .arg(root.join(rel_path))
            .output()
            .unwrap();
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    }

    /// The copy with the shapes the scheme cares about: two photos of one second told apart by
    /// the fraction against their old names, and one already named by its date.
    fn staged(name: &str) -> Library {
        let mut library = Library::new(name);
        let root = library.root.clone();
        exiftool(&root, &format!("{HARBOUR}/DSC_0101.JPG"), &["-SubSecTimeOriginal=90"]);
        exiftool(&root, &format!("{HARBOUR}/DSC_0102.JPG"), &["-SubSecTimeOriginal=20"]);
        let taken = library.cache.named().unwrap();
        let at = |rel_path: &str| {
            taken
                .iter()
                .find(|named| named.rel_path == rel_path)
                .and_then(|named| named.taken_at.clone())
                .unwrap()
        };
        let second = at(&format!("{HARBOUR}/DSC_0101.JPG")).replace('-', ":");
        exiftool(
            &root,
            &format!("{HARBOUR}/DSC_0102.JPG"),
            &[&format!("-DateTimeOriginal={second}")],
        );
        exiftool(
            &root,
            &format!("{WEDDING}/DSCF0002.JPG"),
            &["-DateTimeOriginal=2018:10:06 14:02:11"],
        );
        std::fs::rename(
            root.join(format!("{WEDDING}/DSCF0001.JPG")),
            root.join(format!("{WEDDING}/2018-10-06_140211_2.jpg")),
        )
        .unwrap();
        library.rescan();
        library
    }

    fn name_fixes(library: &Library) -> Vec<Fix> {
        fixes::find(&library.cache, Some(&geo()))
            .into_iter()
            .filter(|fix| fix.finder == "file-names")
            .collect()
    }

    /// Every file of the library by its content id where it has one, with its inode and mtime.
    fn files(library: &Library) -> BTreeMap<String, (String, u64, i64)> {
        library
            .cache
            .named()
            .unwrap()
            .into_iter()
            .map(|named| {
                let found = std::fs::metadata(library.root.join(&named.rel_path)).unwrap();
                (
                    named.content_id.unwrap(),
                    (
                        named.rel_path,
                        found.ino(),
                        found.mtime_nsec() + found.mtime() * 1_000_000_000,
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn one_fix_per_folder_with_a_photo_to_name_and_none_for_a_folder_without_a_date() {
        let library = staged("names-find");
        let found = name_fixes(&library);
        let wedding = found
            .iter()
            .find(|fix| fix.key == format!("file-names:{WEDDING}"))
            .unwrap();
        assert_eq!(wedding.photos, 1);
        assert_eq!(
            wedding.lines,
            [("DSCF0002.JPG".to_string(), "2018-10-06_140211.jpg".to_string())]
        );
        assert!(
            !found
                .iter()
                .any(|fix| fix.key == "file-names:China/2008-01-00 Holiday SOUTHTOUR"),
            "its one photo has no date"
        );
    }

    #[test]
    fn a_ticked_folder_is_named_by_date_and_nothing_else_moves() {
        let mut library = staged("names-apply");
        let before = files(&library);
        let found = name_fixes(&library);
        let ticked: Vec<&Fix> = found
            .iter()
            .filter(|fix| {
                [WEDDING, HARBOUR]
                    .iter()
                    .any(|dir| fix.key == format!("file-names:{dir}"))
            })
            .collect();
        assert_eq!(ticked.len(), 2);
        let finder = fixes::finder("file-names").unwrap();
        let mut set = fixes::change_set(finder, &ticked, &library.cache, Some(&geo())).unwrap();
        set.look(&library.root);
        let summary = library.apply(&set);
        assert_eq!((summary.written, summary.refused, summary.failed), (5, 0, 0));

        let after = files(&library);
        assert_eq!(before.len(), after.len());
        let path = |content: &String| after[content].0.clone();
        for (content, (was, inode, mtime)) in &before {
            let (now, now_inode, now_mtime) = &after[content];
            assert_eq!((inode, mtime), (now_inode, now_mtime), "{was} is the same file");
            if !was.starts_with(WEDDING) && !was.starts_with(HARBOUR) {
                assert_eq!(was, now, "outside the ticked folders nothing moves");
            }
        }
        let content_of = |rel_path: &str| {
            before
                .iter()
                .find(|(_, (was, _, _))| was == rel_path)
                .map(|(content, _)| content.clone())
                .unwrap()
        };
        assert_eq!(
            path(&content_of(&format!("{WEDDING}/DSCF0002.JPG"))),
            format!("{WEDDING}/2018-10-06_140211.jpg")
        );
        assert_eq!(
            path(&content_of(&format!("{WEDDING}/2018-10-06_140211_2.jpg"))),
            format!("{WEDDING}/2018-10-06_140211_2.jpg")
        );
        let later = path(&content_of(&format!("{HARBOUR}/DSC_0101.JPG")));
        let earlier = path(&content_of(&format!("{HARBOUR}/DSC_0102.JPG")));
        assert!(later.ends_with("_2.jpg"), "{later}");
        assert_eq!(earlier, later.replace("_2.jpg", ".jpg"));
        for rel_path in [&later, &earlier] {
            assert!(library.root.join(rel_path).is_file(), "{rel_path} is on disk");
        }

        library.rescan();
        let again = name_fixes(&library);
        assert!(
            !again.iter().any(|fix| [WEDDING, HARBOUR]
                .iter()
                .any(|dir| fix.key == format!("file-names:{dir}"))),
            "{again:?}"
        );
    }

    #[test]
    fn a_photo_whose_people_only_immich_knows_waits_for_them() {
        let library = staged("names-people");
        let fake = fake::FakeImmich::serve(fake::Data::over(&library.root));
        let mut client = immich::Client::new(&fake.url, fake::KEY).unwrap();
        immich::fetch(
            &mut client,
            &immich::beside(library.cache.file()),
            None,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();

        let found = name_fixes(&library);
        let ben = found.iter().find(|fix| fix.key == format!("file-names:{BEN}")).unwrap();
        let waits = ben
            .lines
            .iter()
            .find(|(name, _)| name == "Waits")
            .map(|(_, why)| why.clone());
        assert_eq!(
            waits.as_deref(),
            Some("Immich names Ben in it and the file does not: write the people first")
        );
        let finder = fixes::finder("file-names").unwrap();
        let set = fixes::change_set(finder, &[ben], &library.cache, None).unwrap();
        assert!(
            set.rows.iter().all(|row| matches!(row.verdict, Row::Refused(_))),
            "{:?}",
            set.rows
                .iter()
                .map(|row| (&row.rel_path, &row.verdict))
                .collect::<Vec<_>>()
        );
    }
}
