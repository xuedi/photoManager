//! Folders inside an event, where the layout does not allow them, flattened into the event: every
//! photo of a sub-folder moves up into its event folder, named by its date, and the folder it
//! leaves empty goes. Its name is not kept anywhere. That is safe only once the photos say what
//! the folder did, so a sub-folder is flattened when every photo in it has a date and a position,
//! and nothing else lies in it.

use std::collections::{BTreeMap, HashMap};

use crate::cache::Cache;
use crate::changeset::Wanted;
use crate::layout::IN_A_SUB_FOLDER;
use crate::names::{self, Photo, Verdict};
use crate::tools::folders::PEOPLE_FIRST;
use crate::tools::people;
use crate::write::Move;

/// One sub-folder whose photos can go up into their event.
#[derive(Debug, Clone)]
pub struct SubFolder {
    /// The folder the photos are in now, relative to the library.
    pub dir: String,
    /// Their event's folder, where they go.
    pub event: String,
    pub wanted: Vec<Wanted>,
    /// Why its photos wait for their people, when they do.
    pub waits: Option<String>,
}

impl SubFolder {
    pub fn photos(&self) -> usize {
        self.wanted.len()
    }
}

/// A sub-folder that cannot go up yet, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub dir: String,
    pub photos: usize,
    pub why: String,
}

struct Inside {
    rel_path: String,
    event: String,
    content_id: Option<String>,
    taken_at: Option<String>,
    sub_second: Option<String>,
    placed: bool,
}

/// Every sub-folder the layout does not allow, in path order: those that can go up, and those
/// held back with why. Nothing when the layout allows sub-folders.
pub fn sub_folders(cache: &Cache) -> Result<(Vec<SubFolder>, Vec<Held>), String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let inside = inside(cache).map_err(failed)?;
    let mut by_dir: BTreeMap<String, Vec<Inside>> = BTreeMap::new();
    for photo in inside {
        by_dir
            .entry(parent(&photo.rel_path).to_string())
            .or_default()
            .push(photo);
    }
    let mut on_disk: HashMap<String, Vec<String>> = HashMap::new();
    let mut others_at: HashMap<String, usize> = HashMap::new();
    let photo_paths: std::collections::HashSet<String> = cache.paths().map_err(failed)?.into_iter().collect();
    for rel_path in cache.files().map_err(failed)? {
        let dir = parent(&rel_path).to_string();
        if !photo_paths.contains(&rel_path) {
            *others_at.entry(dir.clone()).or_default() += 1;
        }
        on_disk.entry(dir).or_default().push(name(&rel_path).to_string());
    }

    let mut held = Vec::new();
    let mut by_event: BTreeMap<String, Vec<(String, Vec<Inside>)>> = BTreeMap::new();
    for (dir, photos) in by_dir {
        let undated = photos.iter().filter(|photo| photo.taken_at.is_none()).count();
        let unplaced = photos.iter().filter(|photo| !photo.placed).count();
        let unread = photos.iter().filter(|photo| photo.content_id.is_none()).count();
        let others = others_at.get(&dir).copied().unwrap_or_default();
        let why = [
            (unplaced, "without a position"),
            (undated, "without a date"),
            (unread, "whose image data the scan could not read"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{} {what}", counted(count)))
        .chain((others > 0).then(|| match others {
            1 => "1 file that is not a photo".to_string(),
            others => format!("{others} files that are not photos"),
        }))
        .collect::<Vec<_>>();
        if !why.is_empty() {
            held.push(Held {
                dir,
                photos: photos.len(),
                why: why.join(", "),
            });
            continue;
        }
        let event = photos[0].event.clone();
        by_event.entry(event).or_default().push((dir, photos));
    }

    let mut found = Vec::new();
    for (event, dirs) in by_event {
        let incoming: Vec<(&String, &Inside)> = dirs
            .iter()
            .flat_map(|(dir, photos)| photos.iter().map(move |photo| (dir, photo)))
            .collect();
        let judged: Vec<Photo> = incoming
            .iter()
            .map(|(_, photo)| Photo {
                name: name(&photo.rel_path).to_string(),
                taken_at: photo.taken_at.clone(),
                sub_second: photo.sub_second.clone(),
            })
            .collect();
        let verdicts = names::plan_into(&judged, on_disk.get(&event).map(Vec::as_slice).unwrap_or_default());
        let mut folders: BTreeMap<&String, SubFolder> = BTreeMap::new();
        for ((dir, photo), verdict) in incoming.into_iter().zip(verdicts) {
            let folder = folders.entry(dir).or_insert_with(|| SubFolder {
                dir: dir.clone(),
                event: event.clone(),
                wanted: Vec::new(),
                waits: None,
            });
            let Verdict::Rename(to) = verdict else {
                return Err(format!("{} has no date after all", photo.rel_path));
            };
            let moved = Move {
                from: photo.rel_path.clone(),
                to: format!("{event}/{to}"),
                photos: vec![(
                    photo.rel_path.clone(),
                    photo.content_id.clone().expect("held back without one"),
                )],
            };
            folder.wanted.push(Wanted::moving(moved, None));
        }
        found.extend(folders.into_values());
    }

    let moving: Vec<String> = found
        .iter()
        .flat_map(|folder| folder.wanted.iter().map(|one| one.rel_path.clone()))
        .collect();
    if let Some(untold) = people::untold(cache, &moving)? {
        for folder in &mut found {
            for one in &mut folder.wanted {
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
    Ok((found, held))
}

fn inside(cache: &Cache) -> rusqlite::Result<Vec<Inside>> {
    let mut statement = cache.connection().prepare(
        "SELECT rel_path, event_dir, content_id, taken_at,
            coalesce(nullif(trim(json_extract(raw, '$.\"Exif.Photo.SubSecTimeOriginal\"')), ''),
                nullif(trim(json_extract(raw, '$.SubSecTimeOriginal')), '')),
            gps_lat IS NOT NULL AND gps_lon IS NOT NULL
         FROM photo
         WHERE event_dir IS NOT NULL AND id IN (SELECT photo_id FROM unfit WHERE kind = ?1)
         ORDER BY rel_path",
    )?;
    let rows = statement.query_map([IN_A_SUB_FOLDER], |row| {
        Ok(Inside {
            rel_path: row.get(0)?,
            event: row.get(1)?,
            content_id: row.get(2)?,
            taken_at: row.get(3)?,
            sub_second: row.get(4)?,
            placed: row.get(5)?,
        })
    })?;
    rows.collect()
}

fn parent(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn name(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or(rel_path, |(_, name)| name)
}

fn counted(photos: usize) -> String {
    match photos {
        1 => "1 photo".to_string(),
        _ => format!("{photos} photos"),
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::fixes;
    use crate::layout::Layout;
    use crate::tools::testing::{Library, geo};

    const CANAL: &str = "Germany/2018-05-12 Canal Tour";
    const EVENING: &str = "Germany/2018-05-12 Canal Tour/Evening";

    fn held(found: &[Held]) -> Vec<(&str, &str)> {
        found
            .iter()
            .map(|held| (held.dir.as_str(), held.why.as_str()))
            .collect()
    }

    fn flatten_ticked(library: &mut Library) {
        let found = fixes::find(&library.cache, Some(&geo()));
        let ticked: Vec<&fixes::Fix> = found.iter().filter(|fix| fix.finder == "sub-folders").collect();
        let finder = fixes::finder("sub-folders").unwrap();
        let set = fixes::change_set(finder, &ticked, &library.cache, Some(&geo())).unwrap();
        library.apply(&set);
        library.rescan();
    }

    #[test]
    fn a_sub_folder_goes_up_only_once_every_photo_says_when_and_where() {
        let library = Library::new("flatten-find");
        let (found, waiting) = sub_folders(&library.cache).unwrap();
        let [evening] = found.as_slice() else {
            panic!("{found:?}");
        };
        assert_eq!((evening.dir.as_str(), evening.event.as_str()), (EVENING, CANAL));
        let moved = evening.wanted[0].moved.as_ref().unwrap();
        assert_eq!(moved.to, format!("{CANAL}/2018-05-12_190000.jpg"));
        assert_eq!(
            held(&waiting),
            [
                ("China/2006-09-00 Besuch Ben/2006-08-21", "1 photo without a position"),
                ("Ireland/2008-10-03 Galway/Kira", "1 photo without a position"),
            ]
        );
    }

    #[test]
    fn flattened_the_photos_are_in_their_event_unchanged_and_the_folder_is_gone() {
        let mut library = Library::new("flatten-apply");
        let before = library.cache.stated(&[format!("{EVENING}/PXL_0002.jpg")]).unwrap();
        let content = before.values().next().unwrap().content_id.clone();
        let bytes = std::fs::read(library.root.join(EVENING).join("PXL_0002.jpg")).unwrap();

        flatten_ticked(&mut library);

        let moved = format!("{CANAL}/2018-05-12_190000.jpg");
        assert!(!library.root.join(EVENING).exists(), "the emptied folder goes");
        assert_eq!(
            std::fs::read(library.root.join(&moved)).unwrap(),
            bytes,
            "not a byte changed"
        );
        let after = library.cache.stated(std::slice::from_ref(&moved)).unwrap();
        assert_eq!(after.values().next().unwrap().content_id, content);
        let (found, waiting) = sub_folders(&library.cache).unwrap();
        assert!(found.is_empty(), "a second look finds nothing");
        assert_eq!(waiting.len(), 2);
    }

    #[test]
    fn a_name_the_event_has_is_never_given_and_another_file_holds_the_folder() {
        let mut library = Library::new("flatten-names");
        std::fs::write(library.root.join(CANAL).join("2018-05-12_190000.JPG"), b"not a photo").unwrap();
        library.rescan();
        let (found, _) = sub_folders(&library.cache).unwrap();
        let moved = found[0].wanted[0].moved.as_ref().unwrap();
        assert_eq!(
            moved.to,
            format!("{CANAL}/2018-05-12_190000_2.jpg"),
            "taken in another case"
        );

        std::fs::write(library.root.join(EVENING).join("notes.txt"), b"evening").unwrap();
        library.rescan();
        let (found, waiting) = sub_folders(&library.cache).unwrap();
        assert!(found.is_empty());
        assert!(held(&waiting).contains(&(EVENING, "1 file that is not a photo")));
    }

    #[test]
    fn allowed_sub_folders_are_left_as_they_are() {
        let mut library = Library::new("flatten-allowed");
        library
            .cache
            .follow_layout(&Layout::read("country/city?/*").unwrap())
            .unwrap();
        let (found, waiting) = sub_folders(&library.cache).unwrap();
        assert!(found.is_empty() && waiting.is_empty());
        let fixes = fixes::find(&library.cache, Some(&geo()));
        assert!(!fixes.iter().any(|fix| fix.finder == "sub-folders"));
    }
}
