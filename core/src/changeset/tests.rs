//! Every test here works on a copy of the stand-in library. Nothing in this file, and nothing it
//! calls, may ever be pointed at a real photo.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::journal::Journal;
use crate::metadata::Exiv2;
use crate::scan::{self, Mode};
use crate::thumbs::Thumbs;
use crate::write::{Field, Gps, Taken};

const RATED: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG";
const BARE: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";
const TAGGED: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";
const LOCATED: &str = "Germany/2019-07-13 Sommerfest/img_0657.jpg";

struct Setup {
    base: std::path::PathBuf,
    root: std::path::PathBuf,
    cache: Cache,
    journal: Journal,
}

impl Setup {
    fn new(name: &str) -> Setup {
        let base = std::env::temp_dir().join(format!("photomanager-changeset-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");

        let mut cache = Cache::open(&base.join("cache/cache.db")).unwrap();
        let thumbs = Thumbs::new(base.join("cache/thumbs"));
        scan::run(
            &mut cache,
            &root,
            &Exiv2,
            &thumbs,
            Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .expect("scan the stand-in library");

        Setup {
            journal: Journal::open(&base.join("data/app.db")).unwrap(),
            base,
            root,
            cache,
        }
    }

    fn rescan(&mut self) {
        let thumbs = Thumbs::new(self.base.join("cache/thumbs"));
        scan::run(
            &mut self.cache,
            &self.root,
            &Exiv2,
            &thumbs,
            Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .expect("scan again");
    }

    fn engine(&self) -> Engine {
        Engine::new(&self.root).unwrap()
    }

    fn set(&self, title: &str, wanted: &[Wanted]) -> ChangeSet {
        ChangeSet::build(&self.cache, title, wanted).unwrap()
    }

    /// A rating on every photo in the fixture, whatever each one says today.
    fn rate_everything(&self, stars: i64) -> Vec<Wanted> {
        crate::fixtures::photo_paths()
            .into_iter()
            .map(|path| Wanted::new(path, Change::of([Field::Rating(Some(stars))])))
            .collect()
    }

    fn apply(&mut self, set: &ChangeSet) -> Summary {
        let mut engine = self.engine();
        apply(
            set,
            &mut engine,
            &mut self.journal,
            &mut self.cache,
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("apply the change set")
    }

    fn rating_of(&self, rel_path: &str) -> Option<i64> {
        let out = std::process::Command::new("exiftool")
            .args(["-s3", "-n", "-XMP-xmp:Rating"])
            .arg(self.root.join(rel_path))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    fn image_data(&self) -> std::collections::BTreeMap<String, String> {
        let mut all = std::collections::BTreeMap::new();
        for path in crate::fixtures::photo_paths() {
            let bytes = std::fs::read(self.root.join(path)).unwrap();
            all.insert(path.to_string(), crate::identity::content_id(&bytes).unwrap());
        }
        all
    }

    fn leftovers(&self) -> Vec<String> {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".writing-"))
            .collect()
    }
}

// Phase 1 - what a tool would change

#[test]
fn a_change_set_has_a_row_per_photo_and_the_counts_add_up() {
    let setup = Setup::new("counts");
    let set = setup.set("Rate everything", &setup.rate_everything(3));

    assert_eq!(set.rows.len(), crate::fixtures::photo_count());
    let counts = set.counts();
    assert_eq!(counts.photos, crate::fixtures::photo_count());
    assert_eq!(counts.change + counts.nothing + counts.refused, counts.photos);
    assert_eq!(counts.change, counts.photos, "no fixture photo carries a rating yet");
    assert_eq!(
        counts.selected, counts.change,
        "everything that would change starts selected"
    );
    assert!(counts.traffic > 0);
}

#[test]
fn a_photo_that_already_says_it_is_nothing_to_do_and_costs_no_traffic() {
    let mut setup = Setup::new("settled");
    let first = setup.set("Rate one", &[Wanted::new(BARE, Change::of([Field::Rating(Some(4))]))]);
    setup.apply(&first);
    // The write forgot the cache row, so the photo has to be read again before it is known again.
    setup.rescan();

    let again = setup.set(
        "Rate one",
        &[
            Wanted::new(BARE, Change::of([Field::Rating(Some(4))])),
            Wanted::new(RATED, Change::of([Field::Rating(Some(4))])),
        ],
    );
    assert_eq!(again.rows[0].verdict, Verdict::Nothing);
    assert_eq!(again.rows[1].verdict, Verdict::Change);

    let counts = again.counts();
    assert_eq!((counts.change, counts.nothing), (1, 1));
    assert_eq!(counts.traffic, again.rows[1].size, "only the row that changes costs");
}

#[test]
fn a_tag_set_the_photo_already_carries_is_nothing_to_do() {
    let mut setup = Setup::new("tags");
    let tags = vec!["places/inChina/Beijing".to_string(), "mixed/food".to_string()];
    let first = setup.set(
        "Tag it",
        &[Wanted::new(TAGGED, Change::of([Field::Tags(tags.clone())]))],
    );
    assert_eq!(first.rows[0].verdict, Verdict::Change);
    setup.apply(&first);
    setup.rescan();

    let again = setup.set("Tag it", &[Wanted::new(TAGGED, Change::of([Field::Tags(tags)]))]);
    assert_eq!(again.rows[0].verdict, Verdict::Nothing, "{}", again.rows[0].tells());
}

#[test]
fn a_position_and_a_date_read_the_way_a_person_thinks_about_them() {
    let setup = Setup::new("human");
    let set = setup.set(
        "Move it",
        &[
            Wanted::new(
                LOCATED,
                Change::of([
                    Field::Gps(Some(Gps {
                        lat: 39.90420,
                        lon: 116.40740,
                        altitude: None,
                    })),
                    Field::Taken(Some(Taken {
                        at: "2019-07-13 18:20:00".to_string(),
                        offset: Some("+02:00".to_string()),
                    })),
                ]),
            ),
            Wanted::new(BARE, Change::of([Field::Gps(None)])),
        ],
    );

    let told = set.rows[0].tells();
    assert!(
        told.contains("location: 53.55110, 9.99370 -> 39.90420, 116.40740"),
        "{told}"
    );
    assert!(
        told.contains("date: 2019-07-13 18:20:00 -> 2019-07-13 18:20:00 +02:00"),
        "{told}"
    );
    assert_eq!(set.rows[1].tells(), "location: none -> none");
    assert_eq!(set.rows[1].verdict, Verdict::Nothing, "it has no position to take away");
}

#[test]
fn a_field_the_cache_does_not_keep_says_so_instead_of_guessing() {
    let setup = Setup::new("unknown");
    let set = setup.set("Drop the label", &[Wanted::new(BARE, Change::of([Field::DropLabel]))]);
    assert_eq!(set.rows[0].tells(), "label: unknown -> none");
    assert_eq!(
        set.rows[0].verdict,
        Verdict::Change,
        "what the cache cannot answer is left to the write, which skips it"
    );
}

#[test]
fn deselecting_rows_changes_the_counts_and_the_traffic() {
    let setup = Setup::new("select");
    let mut set = setup.set("Rate everything", &setup.rate_everything(3));
    let all = set.counts();
    assert_eq!(all.selected, all.change);

    assert!(set.select(0, false));
    let fewer = set.counts();
    assert_eq!(fewer.selected, all.selected - 1);
    assert_eq!(fewer.traffic, all.traffic - set.rows[0].size);
    assert_eq!(fewer.change, all.change, "deselecting does not change the verdict");

    set.select_none();
    assert_eq!(set.counts().selected, 0);
    assert_eq!(set.counts().traffic, 0);

    set.select_all();
    assert_eq!(set.counts(), all);
}

#[test]
fn a_photo_the_cache_does_not_know_is_refused_with_a_reason() {
    let setup = Setup::new("stranger");
    let mut set = setup.set(
        "Rate a stranger",
        &[Wanted::new(
            "nowhere/Stranger.JPG",
            Change::of([Field::Rating(Some(3))]),
        )],
    );

    match &set.rows[0].verdict {
        Verdict::Refused(why) => assert!(why.contains("scan the library first"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(set.counts().refused, 1);
    assert_eq!(set.counts().traffic, 0, "a refusal costs nothing");
    assert!(!set.select(0, true), "a refused row cannot be selected");
    assert!(set.targets(&setup.root).is_empty());
}

#[test]
fn an_intent_that_cannot_be_written_is_refused_before_anything_is_read() {
    let setup = Setup::new("bad-intent");
    let set = setup.set(
        "Rate it far too well",
        &[Wanted::new(BARE, Change::of([Field::Rating(Some(9))]))],
    );
    match &set.rows[0].verdict {
        Verdict::Refused(why) => assert!(why.contains("between 0 and 5"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn building_one_reads_no_photo_file_and_writes_nothing() {
    let setup = Setup::new("no-files");
    let before = setup.image_data();
    let wanted = setup.rate_everything(3);

    // With the photos gone a preview that touched a file could not possibly work.
    std::fs::rename(&setup.root, setup.base.join("moved-away")).unwrap();
    let set = setup.set("Rate everything", &wanted);
    assert_eq!(set.counts().change, crate::fixtures::photo_count());
    assert!(set.counts().traffic > 0, "the sizes come from the cache");

    std::fs::rename(setup.base.join("moved-away"), &setup.root).unwrap();
    assert_eq!(setup.image_data(), before);
    assert!(
        setup.journal.passes(10).unwrap().is_empty(),
        "a preview journals nothing"
    );
}

// Phase 2 - the exact diff of one photo

#[test]
fn the_exact_diff_names_every_tag_a_write_then_changes() {
    let mut setup = Setup::new("exact");
    let set = setup.set("Rate one", &[Wanted::new(BARE, Change::of([Field::Rating(Some(4))]))]);
    let mut engine = setup.engine();
    let exact = set.exact(0, &mut engine).expect("a dry run");

    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].tag, "XMP-xmp:Rating");
    assert_eq!(exact[0].now, None);
    assert_eq!(exact[0].then, Some(serde_json::Value::from(4)));
    assert_eq!(exact[0].tells(), "none -> 4");
    drop(engine);

    let summary = setup.apply(&set);
    assert_eq!(summary.written, 1);
    let entries = setup.journal.entries(summary.batch).unwrap();
    let swaps: Vec<&str> = entries[0].swaps.iter().map(|swap| swap.tag.as_str()).collect();
    assert_eq!(swaps, ["XMP-xmp:Rating"], "the write changed what the dry run named");
    assert_eq!(entries[0].swaps[0].old, None);
    assert_eq!(entries[0].swaps[0].new.as_deref(), Some("4"));
}

#[test]
fn the_exact_diff_of_a_tag_write_names_all_five_fields() {
    let setup = Setup::new("exact-tags");
    let set = setup.set(
        "Tag one",
        &[Wanted::new(
            BARE,
            Change::of([Field::Tags(vec!["mixed/food".to_string()])]),
        )],
    );
    let mut engine = setup.engine();
    let exact = set.exact(0, &mut engine).expect("a dry run");

    let tags: Vec<&str> = exact.iter().map(|one| one.tag.as_str()).collect();
    assert_eq!(
        tags,
        [
            "XMP-digiKam:TagsList",
            "XMP-lr:HierarchicalSubject",
            "XMP-microsoft:LastKeywordXMP",
            "XMP-dc:Subject",
            "IPTC:Keywords",
        ]
    );
    assert_eq!(exact[0].tells(), "none -> mixed, mixed/food");
}

#[test]
fn a_dry_run_writes_nothing_journals_nothing_and_leaves_nothing_behind() {
    let setup = Setup::new("dry");
    let before = setup.image_data();
    let set = setup.set("Rate everything", &setup.rate_everything(5));

    let mut engine = setup.engine();
    for index in 0..set.rows.len() {
        set.exact(index, &mut engine).expect("a dry run");
    }

    assert_eq!(setup.image_data(), before, "a dry run moved image data");
    assert!(setup.journal.passes(10).unwrap().is_empty());
    assert!(setup.leftovers().is_empty(), "{:?}", setup.leftovers());
}

#[test]
fn a_photo_that_needs_nothing_has_an_empty_diff() {
    let mut setup = Setup::new("dry-nothing");
    let set = setup.set("Rate one", &[Wanted::new(BARE, Change::of([Field::Rating(Some(4))]))]);
    setup.apply(&set);

    let mut engine = setup.engine();
    let again = set.exact(0, &mut engine).expect("a dry run");
    assert!(again.is_empty(), "the write already did it: {again:?}");
}

#[test]
fn a_dry_run_of_a_photo_that_moved_under_us_comes_back_as_a_reason() {
    let setup = Setup::new("dry-moved");
    let mut set = setup.set("Rate one", &[Wanted::new(BARE, Change::of([Field::Rating(Some(4))]))]);
    set.rows[0].content_id = "0123456789abcdef0123456789abcdef".to_string();

    let mut engine = setup.engine();
    let why = set.exact(0, &mut engine).unwrap_err();
    assert!(why.contains("not what this change was built against"), "{why}");
}

// Phase 3 - applying it

#[test]
fn applying_writes_the_selected_rows_and_leaves_the_rest_alone() {
    let mut setup = Setup::new("apply");
    let mut set = setup.set("Rate everything", &setup.rate_everything(3));
    set.select_none();
    assert!(set.select(0, true));
    assert!(set.select(1, true));
    let kept: Vec<String> = set.rows[..2].iter().map(|row| row.rel_path.clone()).collect();
    let promised = set.counts();

    let summary = setup.apply(&set);
    assert_eq!(summary.written, 2);
    assert_eq!(summary.photos(), promised.selected, "exactly what was promised");
    assert!(!summary.cancelled);

    for path in crate::fixtures::photo_paths() {
        let expected = kept.contains(&path.to_string()).then_some(3);
        assert_eq!(setup.rating_of(path), expected, "{path}");
    }
}

#[test]
fn the_rows_carry_what_became_of_them() {
    let mut setup = Setup::new("settle");
    let mut set = setup.set("Rate two", &setup.rate_everything(2)[..2]);
    let summary = setup.apply(&set);
    set.settle(&summary);

    assert_eq!(set.rows[0].verdict, Verdict::Done(Outcome::Written));
    assert_eq!(set.rows[0].verdict.tells(), "written");
    let counts = set.counts();
    assert_eq!(
        (counts.written, counts.change, counts.selected, counts.traffic),
        (2, 0, 0, 0)
    );
}

#[test]
fn a_photo_edited_between_the_preview_and_the_apply_is_refused_not_written() {
    let mut setup = Setup::new("stale");
    let mut set = setup.set("Rate one", &[Wanted::new(BARE, Change::of([Field::Rating(Some(4))]))]);
    set.rows[0].content_id = "0123456789abcdef0123456789abcdef".to_string();

    let summary = setup.apply(&set);
    assert_eq!((summary.written, summary.refused), (0, 1));
    assert_eq!(setup.rating_of(BARE), None, "the photo was left alone");
}

#[test]
fn a_cancelled_apply_stops_between_photos_and_leaves_the_journal_consistent() {
    let mut setup = Setup::new("cancel");
    let set = setup.set("Rate everything", &setup.rate_everything(1));
    let cancel = AtomicBool::new(false);
    let mut engine = setup.engine();

    let summary = apply(
        &set,
        &mut engine,
        &mut setup.journal,
        &mut setup.cache,
        &|done, _| {
            if done >= 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .unwrap();

    assert!(summary.cancelled);
    assert_eq!(summary.written, 2, "it stopped between photos, not inside one");
    assert!(setup.journal.unfinished().unwrap().is_empty(), "the batch was closed");
    assert_eq!(setup.journal.written(summary.batch).unwrap().len(), 2);
    assert!(setup.leftovers().is_empty());
}

#[test]
fn undo_puts_the_last_applied_change_set_back_and_refuses_a_second_time() {
    let mut setup = Setup::new("undo");
    assert!(undoable(&setup.journal).unwrap().is_none(), "nothing to take back yet");

    let set = setup.set("Rate two", &setup.rate_everything(5)[..2]);
    let applied = setup.apply(&set);
    assert_eq!(setup.rating_of(crate::fixtures::photo_paths()[0]), Some(5));
    assert_eq!(
        undoable(&setup.journal).unwrap().map(|pass| pass.id),
        Some(applied.batch)
    );

    let mut engine = setup.engine();
    let undone = undo_last(
        &mut engine,
        &mut setup.journal,
        &mut setup.cache,
        &|_, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(undone.written, 2);
    for path in &crate::fixtures::photo_paths()[..2] {
        assert_eq!(setup.rating_of(path), None, "{path} kept its new rating");
    }

    assert!(undoable(&setup.journal).unwrap().is_none());
    let refused = undo_last(
        &mut engine,
        &mut setup.journal,
        &mut setup.cache,
        &|_, _| {},
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(refused.to_string().contains("nothing to take back"), "{refused}");
}

#[test]
fn applying_nothing_is_refused_rather_than_journaled() {
    let mut setup = Setup::new("empty");
    let mut set = setup.set("Rate everything", &setup.rate_everything(3));
    set.select_none();

    let mut engine = setup.engine();
    let refused = apply(
        &set,
        &mut engine,
        &mut setup.journal,
        &mut setup.cache,
        &|_, _| {},
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(refused.to_string().contains("no photo is selected"), "{refused}");
    assert!(setup.journal.passes(10).unwrap().is_empty());
}
