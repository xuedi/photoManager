//! The fixes the user set aside: left out of the list of suggestions until brought back.
//!
//! Which fixes they are is a tool setting, kept by the key a fix has for as long as it is found,
//! with the title it had and when it was set aside. Nothing about a photo is kept here. A key no
//! longer found is dropped when the list is found again, so only what is still there waits aside.

use serde_json::{Value, json};

use crate::clock::now;
use crate::fixes::Fix;
use crate::settings::{Result, Settings};

pub const SETTING: &str = "suggestions.set-aside";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aside {
    pub key: String,
    pub title: String,
    /// When it was set aside, in the one date format.
    pub at: String,
}

/// Every fix set aside, oldest first. A setting that does not read is taken as none.
pub fn read(settings: &Settings) -> Vec<Aside> {
    let Ok(Some(text)) = settings.get(SETTING) else {
        return Vec::new();
    };
    let Ok(Value::Array(kept)) = serde_json::from_str::<Value>(&text) else {
        tracing::warn!("the fixes set aside do not read, none are set aside");
        return Vec::new();
    };
    kept.iter()
        .filter_map(|one| {
            let text = |name: &str| one.get(name)?.as_str().map(str::to_string);
            Some(Aside {
                key: text("key")?,
                title: text("title").unwrap_or_default(),
                at: text("at").unwrap_or_default(),
            })
        })
        .collect()
}

fn keep(settings: &mut Settings, aside: &[Aside]) -> Result<()> {
    if aside.is_empty() {
        return settings.forget(SETTING);
    }
    let kept: Vec<Value> = aside
        .iter()
        .map(|one| json!({ "key": one.key, "title": one.title, "at": one.at }))
        .collect();
    settings.put(SETTING, &Value::Array(kept).to_string())
}

pub fn set_aside(settings: &mut Settings, fix: &Fix) -> Result<()> {
    let mut aside = read(settings);
    if aside.iter().any(|one| one.key == fix.key) {
        return Ok(());
    }
    aside.push(Aside {
        key: fix.key.clone(),
        title: fix.title.clone(),
        at: now(),
    });
    keep(settings, &aside)
}

pub fn bring_back(settings: &mut Settings, key: &str) -> Result<()> {
    let mut aside = read(settings);
    let before = aside.len();
    aside.retain(|one| one.key != key);
    if aside.len() == before {
        return Ok(());
    }
    keep(settings, &aside)
}

/// Splits what was found into the fixes offered and the fixes set aside, in the order found, and
/// drops from the setting every key that was not found.
pub fn split(settings: &mut Settings, found: Vec<Fix>) -> Result<(Vec<Fix>, Vec<Fix>)> {
    let aside = read(settings);
    let (set_aside, offered): (Vec<Fix>, Vec<Fix>) = found
        .into_iter()
        .partition(|fix| aside.iter().any(|one| one.key == fix.key));
    if set_aside.len() < aside.len() {
        let still: Vec<Aside> = aside
            .into_iter()
            .filter(|one| set_aside.iter().any(|fix| fix.key == one.key))
            .collect();
        keep(settings, &still)?;
    }
    Ok((offered, set_aside))
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::tools::testing::{Library, geo};

    fn settings(name: &str) -> (Settings, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("photomanager-aside-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("app.db");
        (Settings::open(&file).unwrap(), file)
    }

    fn keys(fixes: &[Fix]) -> Vec<&str> {
        fixes.iter().map(|fix| fix.key.as_str()).collect()
    }

    #[test]
    fn a_fix_set_aside_is_not_offered_until_it_is_brought_back() {
        let library = Library::new("aside-split");
        let geo = geo();
        let found = crate::fixes::find(&library.cache, Some(&geo));
        assert!(found.len() > 1, "the fixture has fixes to set aside");
        let chosen = found[1].clone();
        let (mut settings, file) = settings("split");

        set_aside(&mut settings, &chosen).unwrap();
        set_aside(&mut settings, &chosen).unwrap();
        assert_eq!(read(&settings).len(), 1, "set aside once");
        assert_eq!(read(&settings)[0].at.len(), 19);

        let (offered, aside) = split(&mut settings, found.clone()).unwrap();
        assert_eq!(keys(&aside), [chosen.key.as_str()]);
        assert!(!keys(&offered).contains(&chosen.key.as_str()));
        assert_eq!(offered.len() + 1, found.len());

        drop(settings);
        let mut settings = Settings::open(&file).unwrap();
        assert_eq!(read(&settings)[0].title, chosen.title, "it outlives a reopen");

        bring_back(&mut settings, &chosen.key).unwrap();
        let (offered, aside) = split(&mut settings, found.clone()).unwrap();
        assert!(aside.is_empty());
        assert_eq!(keys(&offered), keys(&found), "offered again, in its place");
        assert_eq!(settings.get(SETTING).unwrap(), None);
    }

    #[test]
    fn a_key_no_longer_found_is_dropped() {
        let library = Library::new("aside-prune");
        let geo = geo();
        let found = crate::fixes::find(&library.cache, Some(&geo));
        let (mut settings, _) = settings("prune");
        set_aside(&mut settings, &found[0]).unwrap();
        let mut gone = found[0].clone();
        gone.key = "folders:Nowhere/2001-01-01 Never".to_string();
        set_aside(&mut settings, &gone).unwrap();

        let (_, aside) = split(&mut settings, found).unwrap();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            read(&settings).iter().map(|one| one.key.as_str()).collect::<Vec<_>>(),
            [aside[0].key.as_str()]
        );
    }

    #[test]
    fn setting_aside_writes_nothing_in_the_library() {
        let library = Library::new("aside-nothing-written");
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
        let before = stamps();
        let found = crate::fixes::find(&library.cache, Some(&geo()));
        let (mut settings, _) = settings("nothing-written");
        set_aside(&mut settings, &found[0]).unwrap();
        split(&mut settings, found.clone()).unwrap();
        bring_back(&mut settings, &found[0].key).unwrap();
        assert_eq!(stamps(), before);
    }

    #[test]
    fn a_setting_that_does_not_read_is_none() {
        let (mut settings, _) = settings("unread");
        settings.put(SETTING, "not a list").unwrap();
        assert!(read(&settings).is_empty());
    }
}
