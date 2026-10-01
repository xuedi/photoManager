//! Suggestions: what the app is sure it can fix, one fix at a time. A fix is one tag rule, one
//! person, one places tag, one event: small enough to be ticked or not on its own, and worked out
//! to the end, so ticking it is the whole decision.
//!
//! Nothing about a fix is kept but whether the user set it aside ([`crate::aside`]). The list is
//! found again after every write, and a fix that was applied is gone because the photos now say it.
//!
//! The ticked fixes are applied finder by finder, in the order of [`FINDERS`], each finder one
//! pass, with the library read again in between: the tags before the people, the
//! people before any folder moves, so the people gate lets an event go once its people are
//! written.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cache::Cache;
use crate::changeset::{self, ChangeSet, Wanted};
use crate::filter::Filter;
use crate::geo::Geo;
use crate::names;
use crate::scope::Scope;
use crate::tags::{Rule, Rules};
use crate::tools::folders::{self, FolderMigration};
use crate::tools::gps_from_event::GpsFromEvent;
use crate::tools::gps_from_places::{self, GpsFromPlacesTag};
use crate::tools::{Answer, Answers, Question, Tool, tag_vocabulary};
use crate::tools::{duplicate_people, people, people_from_tags, place_words};
use crate::write::{self, Engine};

/// One kind of fix, and the pass its ticked fixes are written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finder {
    pub key: &'static str,
    pub title: &'static str,
    /// What its fixes fix, in one line.
    pub fixes: &'static str,
    /// What the pass is called: the title of its change set.
    pub pass: &'static str,
}

/// Every finder, in the order its fixes are applied.
pub const FINDERS: [Finder; 9] = [
    Finder {
        key: "tags",
        title: "Tag Tree",
        fixes: "Tags into the shape the rest of the tree has, and tag fields that disagree",
        pass: "Tidy the tag tree",
    },
    Finder {
        key: "duplicate-people",
        title: "Duplicate People",
        fixes: "Photos that name a person more than once on the same face, named once",
        pass: "Name people once",
    },
    Finder {
        key: "people",
        title: "People",
        fixes: "Who Immich found in a photo, under Immich's name, with a box on each face",
        pass: "Write people from Immich",
    },
    Finder {
        key: "people-from-tags",
        title: "People from Tags",
        fixes: "Photos whose people tag names a person the library knows, as a person without a box",
        pass: "Write people from tags",
    },
    Finder {
        key: "places-from-tags",
        title: "Places from Tags",
        fixes: "Photos without GPS whose places tag names a town exactly",
        pass: "Set GPS from the places tag",
    },
    Finder {
        key: "places-from-events",
        title: "Places from Events",
        fixes: "Photos without GPS in an event whose located photos all stand in one town",
        pass: "Set GPS from the event",
    },
    Finder {
        key: "place-words",
        title: "Place Words from GPS",
        fixes: "Photos with a position and no place in words, given the town, the state and the country",
        pass: "Write the place from the position",
    },
    Finder {
        key: "folders",
        title: "Folders",
        fixes: "Events with one sure folder in the layout",
        pass: "Move events into the folder layout",
    },
    Finder {
        key: "file-names",
        title: "File Names",
        fixes: "Photos not named by the date they were taken",
        pass: "Name photos by their date",
    },
];

pub fn finder(key: &str) -> Option<&'static Finder> {
    FINDERS.iter().find(|finder| finder.key == key)
}

/// One fix: what it is about, the photos it changes, and what it does in a line or two.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    /// Stays the same for as long as the same fix is found.
    pub key: String,
    pub finder: &'static str,
    pub title: String,
    pub detail: String,
    pub photos: usize,
    /// What it changes, a name and what becomes of it, where the title does not say it all.
    pub lines: Vec<(String, String)>,
    what: What,
}

#[derive(Debug, Clone, PartialEq)]
enum What {
    Rule(Rule),
    Tidy,
    Answer {
        question: String,
        answer: Answer,
    },
    /// The folder whose photos are named by their date.
    Names(String),
    /// The person, by the name folded, named once in the photos that name them more than once.
    Doubled(String),
    /// The person, by Immich's id, written into the photos Immich finds them in.
    Person(String),
    /// The person a people tag names, written into its photos without a box.
    Tagged {
        tag: String,
        name: String,
    },
    /// The country, by code, whose photos get the words of their position.
    Country(String),
}

/// Every fix there is in the library, finder by finder. A finder that cannot look is left out,
/// and the log says why.
pub fn find(cache: &Cache, geo: Option<&Geo>) -> Vec<Fix> {
    let mut found = Vec::new();
    for finder in &FINDERS {
        match find_one(finder.key, cache, geo) {
            Ok(fixes) => found.extend(fixes),
            Err(why) => tracing::warn!(finder = finder.key, why, "the finder could not look"),
        }
    }
    found
}

fn whole() -> Scope {
    Scope::Filter(Filter::all())
}

fn find_one(key: &str, cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Fix>, String> {
    Ok(match key {
        "tags" => tag_fixes(cache)?,
        "duplicate-people" => duplicate_people::sure(cache)?.into_iter().map(doubled_fix).collect(),
        "people" => people::sure(cache)?.into_iter().map(person_fix).collect(),
        "people-from-tags" => people_from_tags::sure(cache)?
            .into_iter()
            .map(|found| Fix {
                key: format!("people-from-tags:{}", found.tag),
                finder: "people-from-tags",
                title: found.tag.clone(),
                detail: format!("{}, without a box", found.name),
                photos: found.photos,
                lines: Vec::new(),
                what: What::Tagged {
                    tag: found.tag,
                    name: found.name,
                },
            })
            .collect(),
        "places-from-tags" => gps_from_places::sure(cache, geo)?
            .into_iter()
            .map(|(question, photos)| answered("places-from-tags", question, photos, |answer| answer.tells()))
            .collect(),
        "places-from-events" => sure(&GpsFromEvent, cache, geo)?
            .into_iter()
            .map(|question| {
                let photos = question.photos;
                answered("places-from-events", question, photos, |answer| answer.tells())
            })
            .collect(),
        "place-words" => place_words::sure(cache, geo)?
            .into_iter()
            .map(|country| {
                let shown: Vec<&str> = country.towns.iter().take(SHOWN_TOWNS).map(String::as_str).collect();
                let mut towns = shown.join(", ");
                if country.towns.len() > SHOWN_TOWNS {
                    towns.push_str(&format!(" and {} more", country.towns.len() - SHOWN_TOWNS));
                }
                Fix {
                    key: format!("place-words:{}", country.code),
                    finder: "place-words",
                    title: country.name,
                    detail: "The town, the state and the country, from the position".to_string(),
                    photos: country.photos,
                    lines: vec![("Towns".to_string(), towns)],
                    what: What::Country(country.code),
                }
            })
            .collect(),
        "folders" => folder_fixes(cache, geo)?,
        "file-names" => name_fixes(cache)?,
        other => return Err(format!("there is no finder {other}")),
    })
}

/// The questions about the whole library whose best offer is sure.
fn sure<T: Tool<Settings = Answers>>(tool: &T, cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Question>, String> {
    Ok(tool
        .questions(cache, geo, &whole(), &Answers::default())?
        .into_iter()
        .filter(Question::confirmable)
        .collect())
}

/// A sure question as a fix, answered with its best offer.
fn answered(finder: &'static str, question: Question, photos: usize, detail: impl Fn(&Answer) -> String) -> Fix {
    let answer = question
        .sure()
        .map(|offer| offer.answer.clone())
        .expect("a sure question has a sure offer");
    Fix {
        key: format!("{finder}:{}", question.key),
        finder,
        title: question.title,
        detail: detail(&answer),
        photos,
        lines: Vec::new(),
        what: What::Answer {
            question: question.key,
            answer,
        },
    }
}

fn doubled_fix(found: duplicate_people::Found) -> Fix {
    let mut lines = vec![(
        "Goes".to_string(),
        match found.copies {
            1 => "1 copy".to_string(),
            copies => format!("{copies} copies"),
        },
    )];
    if let Some(why) = found.why {
        lines.push(("Left".to_string(), format!("{}, {why}", counted(found.refused))));
    }
    Fix {
        key: format!("duplicate-people:{}", found.key),
        finder: "duplicate-people",
        title: found.name,
        detail: "Named once, the largest box on the face stays".to_string(),
        photos: found.photos,
        lines,
        what: What::Doubled(found.key),
    }
}

fn person_fix(found: people::Found) -> Fix {
    let mut lines = Vec::new();
    if let Some(why) = found.refused {
        lines.push(("Refused".to_string(), why));
    }
    if found.clashed > 0 {
        lines.push((
            "Left".to_string(),
            format!("{}, Immich names someone else on the same face", counted(found.clashed)),
        ));
    }
    Fix {
        key: format!("people:{}", found.id),
        finder: "people",
        title: found.name,
        detail: "Named as in Immich, with a box on each face".to_string(),
        photos: found.photos,
        lines,
        what: What::Person(found.id),
    }
}

/// The events with one sure folder. One whose move is refused for good - its folder is there
/// already, or another goes there too - is no fix; one that waits for its people from Immich is,
/// since ticking its people too lets it go.
fn folder_fixes(cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Fix>, String> {
    let asked = sure(&FolderMigration, cache, geo)?;
    let answered_all: Vec<(String, String)> = asked
        .iter()
        .filter_map(|question| match question.sure().map(|offer| &offer.answer) {
            Some(Answer::Folder(to)) => Some((question.key.clone(), to.clone())),
            _ => None,
        })
        .collect();
    let refused: HashMap<String, String> = folders::moves(cache, &answered_all)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|wanted| Some((wanted.rel_path, wanted.refused?)))
        .collect();
    let mut fixes = Vec::new();
    for question in asked {
        let why = refused.get(&question.key).cloned();
        if why
            .as_deref()
            .is_some_and(|why| !why.starts_with(folders::PEOPLE_FIRST))
        {
            continue;
        }
        let photos = question.photos;
        let from = question.key.clone();
        let mut fix = answered("folders", question, photos, |answer| answer.tells());
        if let What::Answer {
            answer: Answer::Folder(to),
            ..
        } = &fix.what
        {
            fix.lines = vec![("Now".to_string(), from), ("After".to_string(), to.clone())];
        }
        if let Some(why) = why {
            fix.lines.push(("Waits".to_string(), why));
        }
        fixes.push(fix);
    }
    Ok(fixes)
}

/// How many of the renames of a fix are shown before the rest is only counted.
const SHOWN_RENAMES: usize = 3;
/// How many of the towns of a country are named before the rest is only counted.
const SHOWN_TOWNS: usize = 5;

fn name_fixes(cache: &Cache) -> Result<Vec<Fix>, String> {
    Ok(names::folders(cache)?
        .into_iter()
        .map(|folder| {
            let detail = match folder.undated {
                0 => "Named by the date taken".to_string(),
                1 => "Named by the date taken, 1 without a date keeps its name".to_string(),
                undated => format!("Named by the date taken, {undated} without a date keep their name"),
            };
            let mut lines: Vec<(String, String)> = folder
                .wanted
                .iter()
                .filter_map(|one| one.moved.as_ref())
                .take(SHOWN_RENAMES)
                .map(|moved| (file_name(&moved.from).to_string(), file_name(&moved.to).to_string()))
                .collect();
            if folder.renamed > SHOWN_RENAMES {
                lines.push(("And".to_string(), counted(folder.renamed - SHOWN_RENAMES)));
            }
            if let Some(why) = folder.waits {
                lines.push(("Waits".to_string(), why));
            }
            Fix {
                key: format!("file-names:{}", folder.dir),
                finder: "file-names",
                title: match folder.dir.is_empty() {
                    true => "The library itself".to_string(),
                    false => folder.dir.clone(),
                },
                detail,
                photos: folder.renamed,
                lines,
                what: What::Names(folder.dir),
            }
        })
        .collect())
}

fn counted(photos: usize) -> String {
    match photos {
        1 => "1 photo".to_string(),
        _ => format!("{photos} photos"),
    }
}

fn file_name(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or(rel_path, |(_, name)| name)
}

fn tag_fixes(cache: &Cache) -> Result<Vec<Fix>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let mut fixes: Vec<Fix> = tag_vocabulary::shape(cache)
        .map_err(failed)?
        .into_iter()
        .map(|found| Fix {
            key: format!("tags:{}", found.rule.written()),
            finder: "tags",
            title: found.rule.tells(),
            detail: found.why,
            photos: found.photos,
            lines: Vec::new(),
            what: What::Rule(found.rule),
        })
        .collect();
    let untidy = cache
        .tagged(&whole().paths(cache).map_err(failed)?)
        .map_err(failed)?
        .iter()
        .filter(|photo| photo.untidy)
        .count();
    if untidy > 0 {
        fixes.push(Fix {
            key: "tags:tidy".to_string(),
            finder: "tags",
            title: "Tag fields that disagree".to_string(),
            detail: "Every tag field written the same, with the tags the photo has".to_string(),
            photos: untidy,
            lines: Vec::new(),
            what: What::Tidy,
        });
    }
    Ok(fixes)
}

/// The fixes of one finder as one change set over the library as it is now. A fix whose photos
/// were fixed since it was found changes nothing and is dropped by the change set.
pub fn change_set(finder: &Finder, fixes: &[&Fix], cache: &Cache, geo: Option<&Geo>) -> Result<ChangeSet, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let mut answers = Answers::default();
    let mut rules = Rules::default();
    let mut tidy = false;
    let mut named = BTreeSet::new();
    let mut persons = BTreeSet::new();
    let mut doubled = BTreeSet::new();
    let mut tagged = std::collections::BTreeMap::new();
    let mut countries = BTreeSet::new();
    for fix in fixes.iter().filter(|fix| fix.finder == finder.key) {
        match &fix.what {
            What::Rule(rule) => {
                if let Err(why) = rules.add(rule.clone()) {
                    tracing::warn!(fix = fix.key, why, "the rule was left out");
                }
            }
            What::Tidy => tidy = true,
            What::Answer { question, answer } => answers.set(question, Some(answer.clone())),
            What::Names(dir) => {
                named.insert(dir.clone());
            }
            What::Doubled(key) => {
                doubled.insert(key.clone());
            }
            What::Person(id) => {
                persons.insert(id.clone());
            }
            What::Tagged { tag, name } => {
                tagged.insert(tag.clone(), name.clone());
            }
            What::Country(code) => {
                countries.insert(code.clone());
            }
        }
    }
    let wanted: Vec<Wanted> = match finder.key {
        "tags" => tag_vocabulary::renamed(cache, &whole(), &rules, tidy).map_err(failed)?,
        "duplicate-people" => duplicate_people::wanted(cache, &doubled, &whole()).map_err(failed)?,
        "people" => people::wanted(cache, &persons, &whole()).map_err(failed)?,
        "people-from-tags" => people_from_tags::wanted(cache, &tagged, &whole()).map_err(failed)?,
        "places-from-tags" => GpsFromPlacesTag
            .wanted(cache, geo, &whole(), &answers)
            .map_err(failed)?,
        "places-from-events" => GpsFromEvent.wanted(cache, geo, &whole(), &answers).map_err(failed)?,
        "place-words" => place_words::wanted(cache, geo, &countries, &whole()).map_err(failed)?,
        "folders" => FolderMigration.wanted(cache, geo, &whole(), &answers).map_err(failed)?,
        "file-names" => names::folders(cache)?
            .into_iter()
            .filter(|folder| named.contains(&folder.dir))
            .flat_map(|folder| folder.wanted)
            .collect(),
        other => return Err(format!("there is no finder {other}")),
    };
    ChangeSet::build(cache, finder.pass, &wanted).map_err(failed)
}

/// The question through one apply: once the answer is to write anyway every one that follows,
/// it is not asked again, and the answer goes when the apply does.
fn for_this_apply(
    ask: &mut dyn FnMut(&changeset::Asked) -> changeset::Anyway,
) -> impl FnMut(&changeset::Asked) -> changeset::Anyway + '_ {
    let mut always = false;
    move |asked| {
        if always {
            return changeset::Anyway::Write;
        }
        let answer = ask(asked);
        always = answer == changeset::Anyway::WriteAll;
        answer
    }
}

/// What one finder's pass came to.
#[derive(Debug, Clone)]
pub struct Pass {
    pub finder: &'static str,
    pub summary: write::Summary,
}

/// Writes the ticked fixes, finder by finder, each as one pass, and reads the library again after
/// each pass that another one follows: the next finder works on what the photos say now. A
/// cancel stops after the pass it came in.
///
/// A pass whose photos ExifTool doubted asks about them before the next pass. An answer to write
/// anyway for every one that follows holds for the rest of this apply and no longer.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    fixes: &[Fix],
    cache: &mut Cache,
    geo: Option<&Geo>,
    engine: &mut Engine,
    rescan: &mut dyn FnMut(&mut Cache) -> Result<(), String>,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
    ask: &mut dyn FnMut(&changeset::Asked) -> changeset::Anyway,
) -> Result<Vec<Pass>, String> {
    let ticked: BTreeSet<&str> = fixes.iter().map(|fix| fix.finder).collect();
    let chosen: Vec<&Finder> = FINDERS.iter().filter(|finder| ticked.contains(finder.key)).collect();
    let mut passes = Vec::new();
    let mut stale = false;
    let mut asking = for_this_apply(ask);
    let last = chosen.len().saturating_sub(1);
    for (at, finder) in chosen.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if stale {
            rescan(cache)?;
            stale = false;
        }
        let own: Vec<&Fix> = fixes.iter().filter(|fix| fix.finder == finder.key).collect();
        let mut set = change_set(finder, &own, cache, geo)?;
        set.look(engine.library());
        if set.counts().selected == 0 {
            tracing::info!(finder = finder.key, "nothing left to write");
            continue;
        }
        let summary = changeset::apply_asking(&set, engine, cache, progress, cancel, at < last, &mut asking)
            .map_err(|error| error.to_string())?;
        stale = summary.written > 0;
        passes.push(Pass {
            finder: finder.key,
            summary,
        });
    }
    Ok(passes)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::metadata::Exiv2;
    use crate::scan::{self, Mode};
    use crate::thumbs::Thumbs;
    use crate::tools::testing::{Library, geo};

    fn keys(fixes: &[Fix], finder: &str) -> Vec<String> {
        fixes
            .iter()
            .filter(|fix| fix.finder == finder)
            .map(|fix| fix.key.clone())
            .collect()
    }

    fn apply_ticked(library: &mut Library, ticked: &[Fix]) -> Vec<Pass> {
        let geo = geo();
        let mut engine = Engine::new(&library.root).unwrap();
        let root = library.root.clone();
        let thumbs = Thumbs::new(root.parent().unwrap().join("cache/thumbs"));
        let mut rescan = |cache: &mut Cache| -> Result<(), String> {
            scan::run(
                cache,
                &root,
                &Exiv2,
                &thumbs,
                Mode::Reconcile,
                &|_| {},
                &AtomicBool::new(false),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        };
        apply(
            ticked,
            &mut library.cache,
            Some(&geo),
            &mut engine,
            &mut rescan,
            &|_, _| {},
            &AtomicBool::new(false),
            &mut |_| changeset::Anyway::Skip,
        )
        .unwrap()
    }

    #[test]
    fn each_finder_finds_one_fix_per_thing_and_writes_nothing() {
        let library = Library::new("fixes-find");
        let stamps = || -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
            walkdir::WalkDir::new(&library.root)
                .sort_by_file_name()
                .into_iter()
                .flatten()
                .map(|entry| {
                    (
                        entry.path().to_path_buf(),
                        entry.metadata().unwrap().modified().unwrap(),
                    )
                })
                .collect()
        };
        let untouched = stamps();
        let fixes = find(&library.cache, Some(&geo()));
        let places = keys(&fixes, "places-from-tags");
        assert_eq!(places.len(), 6, "{places:?}");
        assert!(places.contains(&"places-from-tags:places/inChina/Beijing".to_string()));
        let beijing = fixes
            .iter()
            .find(|fix| fix.key == "places-from-tags:places/inChina/Beijing")
            .unwrap();
        assert_eq!(
            (beijing.title.as_str(), beijing.detail.as_str()),
            ("inChina/Beijing", "Beijing, Beijing, China")
        );

        let folders: Vec<&Fix> = fixes.iter().filter(|fix| fix.finder == "folders").collect();
        let [folder] = folders.as_slice() else {
            panic!("one sure folder: {folders:?}");
        };
        assert_eq!(folder.title, "China/2006-09-00 Besuch Ben");
        assert_eq!(folder.photos, 2);
        assert_eq!(
            folder.lines[0],
            ("Now".to_string(), "China/2006-09-00 Besuch Ben".to_string())
        );

        let order: Vec<&str> = fixes.iter().map(|fix| fix.finder).collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|key| FINDERS.iter().position(|finder| finder.key == *key));
        assert_eq!(order, sorted, "finder by finder, in the order they are applied");
        assert_eq!(stamps(), untouched, "finding writes nothing");
    }

    #[test]
    fn a_ticked_fix_is_written_and_then_found_no_more_and_the_rest_stays() {
        let mut library = Library::new("fixes-apply");
        let fixes = find(&library.cache, Some(&geo()));
        let beijing: Vec<Fix> = fixes
            .iter()
            .filter(|fix| fix.key == "places-from-tags:places/inChina/Beijing")
            .cloned()
            .collect();
        let passes = apply_ticked(&mut library, &beijing);
        let [pass] = passes.as_slice() else {
            panic!("one pass: {passes:?}");
        };
        assert_eq!(pass.finder, "places-from-tags");
        assert_eq!(pass.summary.written, 2);

        library.rescan();
        let again = find(&library.cache, Some(&geo()));
        assert!(!again.iter().any(|fix| fix.key == beijing[0].key), "applied is gone");
        assert_eq!(
            keys(&again, "places-from-tags").len(),
            5,
            "what was not ticked is still there"
        );
    }

    #[test]
    fn a_person_from_a_tag_and_the_words_of_a_position_are_fixes_that_go_once_written() {
        let mut library = Library::new("fixes-persons-words");
        let fixes = find(&library.cache, Some(&geo()));
        assert_eq!(
            keys(&fixes, "people-from-tags"),
            ["people-from-tags:people/family/Anna"]
        );
        let anna = fixes.iter().find(|fix| fix.finder == "people-from-tags").unwrap();
        assert_eq!((anna.detail.as_str(), anna.photos), ("Anna, without a box", 1));
        let words = keys(&fixes, "place-words");
        assert!(words.contains(&"place-words:DE".to_string()), "{words:?}");

        let ticked: Vec<Fix> = fixes
            .iter()
            .filter(|fix| fix.finder == "people-from-tags" || fix.key == "place-words:DE")
            .cloned()
            .collect();
        let passes = apply_ticked(&mut library, &ticked);
        let order: Vec<(&str, usize)> = passes.iter().map(|pass| (pass.finder, pass.summary.refused)).collect();
        assert_eq!(order, [("people-from-tags", 0), ("place-words", 0)]);
        assert!(passes.iter().all(|pass| pass.summary.written > 0), "{passes:?}");

        library.rescan();
        let again = find(&library.cache, Some(&geo()));
        assert!(keys(&again, "people-from-tags").is_empty());
        assert!(!keys(&again, "place-words").contains(&"place-words:DE".to_string()));
    }

    const DOUBTED: &str = "China/2006-09-00 Besuch Ben/doubted.jpg";

    /// Applies the Beijing places tag to a library with a photo whose maker note ExifTool doubts,
    /// answering with `answer`: the questions, the pass and the library after.
    fn apply_doubted(name: &str, answer: changeset::Anyway) -> (Vec<changeset::Asked>, Pass, Library) {
        let mut library = Library::new(name);
        library.add_doubted(DOUBTED, &["places/inChina/Beijing"]);
        let ticked: Vec<Fix> = find(&library.cache, Some(&geo()))
            .into_iter()
            .filter(|fix| fix.key == "places-from-tags:places/inChina/Beijing")
            .collect();
        assert_eq!(ticked.len(), 1);
        assert_eq!(ticked[0].photos, 3, "the two of the fixture and the doubted one");
        let geo = geo();
        let mut engine = Engine::new(&library.root).unwrap();
        let mut asked = Vec::new();
        let passes = apply(
            &ticked,
            &mut library.cache,
            Some(&geo),
            &mut engine,
            &mut |_| Ok(()),
            &|_, _| {},
            &AtomicBool::new(false),
            &mut |question| {
                asked.push(question.clone());
                answer
            },
        )
        .unwrap();
        library.rescan();
        let [pass] = passes.as_slice() else {
            panic!("one pass: {passes:?}");
        };
        (asked, pass.clone(), library)
    }

    fn doubted_says(library: &Library) -> crate::cache::Said {
        library.cache.stated(&[DOUBTED.to_string()]).unwrap()[DOUBTED]
            .said
            .clone()
    }

    #[test]
    fn a_doubted_photo_is_asked_about_once_by_camera_and_left_on_skip() {
        let (asked, pass, library) = apply_doubted("fixes-doubted-skip", changeset::Anyway::Skip);
        let [question] = asked.as_slice() else {
            panic!("asked once: {asked:?}");
        };
        assert_eq!(
            question.doubted,
            [changeset::Doubted {
                camera: Some("OLYMPUS X1".to_string()),
                why: "Truncated MakerNotes directory".to_string(),
                photos: vec![DOUBTED.to_string()],
            }]
        );
        assert!(!question.more_to_come, "the only pass");
        assert_eq!(
            (pass.summary.written, pass.summary.doubted),
            (2, 1),
            "the others are written"
        );
        assert!(doubted_says(&library).gps_lat.is_none(), "left as it was");
    }

    #[test]
    fn a_doubted_photo_written_anyway_is_written_only_if_nothing_is_lost() {
        let (asked, pass, library) = apply_doubted("fixes-doubted-write", changeset::Anyway::Write);
        assert_eq!(asked.len(), 1);
        let left = pass
            .summary
            .outcomes
            .iter()
            .find(|(rel_path, _)| rel_path == DOUBTED)
            .map(|(_, outcome)| outcome.clone());
        assert!(
            matches!(&left, Some(crate::write::Outcome::Failed(why)) if why.contains("would lose")),
            "this maker note would come out shorter: {left:?}"
        );
        assert_eq!((pass.summary.written, pass.summary.doubted), (2, 0));
        assert!(doubted_says(&library).gps_lat.is_none());
    }

    #[test]
    fn write_anyway_for_the_rest_holds_for_this_apply_only() {
        let question = changeset::Asked {
            doubted: Vec::new(),
            more_to_come: true,
        };
        let mut asked = 0;
        let mut answers = [changeset::Anyway::Write, changeset::Anyway::WriteAll].into_iter();
        let mut ask = |_: &changeset::Asked| {
            asked += 1;
            answers.next().unwrap()
        };
        let mut through = for_this_apply(&mut ask);
        let said: Vec<changeset::Anyway> = (0..4).map(|_| through(&question)).collect();
        drop(through);
        assert_eq!(asked, 2, "asked until the answer was every one that follows");
        assert_eq!(
            said,
            [
                changeset::Anyway::Write,
                changeset::Anyway::WriteAll,
                changeset::Anyway::Write,
                changeset::Anyway::Write
            ]
        );
        let mut again = 0;
        let mut ask = |_: &changeset::Asked| {
            again += 1;
            changeset::Anyway::Skip
        };
        let mut next = for_this_apply(&mut ask);
        next(&question);
        drop(next);
        assert_eq!(again, 1, "a new apply asks again");
    }

    #[test]
    fn two_finders_are_two_passes_in_their_order_with_the_library_read_between() {
        let mut library = Library::new("fixes-order");
        let fixes = find(&library.cache, Some(&geo()));
        let mut ticked: Vec<Fix> = fixes.iter().filter(|fix| fix.finder == "folders").cloned().collect();
        ticked.extend(fixes.iter().filter(|fix| fix.finder == "places-from-tags").cloned());
        assert!(ticked.len() > 1);
        let passes = apply_ticked(&mut library, &ticked);
        let order: Vec<&str> = passes.iter().map(|pass| pass.finder).collect();
        assert_eq!(
            order,
            ["places-from-tags", "folders"],
            "places are written before the move"
        );
        assert!(passes.iter().all(|pass| pass.summary.written > 0), "{passes:?}");

        library.rescan();
        let again = find(&library.cache, Some(&geo()));
        assert!(keys(&again, "folders").is_empty());
        assert!(keys(&again, "places-from-tags").is_empty());
    }
}
