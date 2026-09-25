//! What the folders say about a photo, read against the layout the user chose: the levels above
//! the event folder (`Country/City`, `Year/Country`, none at all), then the event folder
//! `YYYY-MM-DD Event`, then any sub-folders. The event folder is found by its date wherever it
//! is, so a library in another layout still has its events - they are only off the layout. A date
//! may have holes (`2006-09-00`), and what does not fit is reported, never guessed.

use std::fmt;

/// What a level above the event folder is named after.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Component {
    Country,
    /// The state or province, as the place data names it.
    Region,
    City,
    /// `2019`, the year of the event.
    Year,
    /// `2019-07`, the year and month of the event.
    Month,
    /// The tag the event's photos carry right under this root: `topics/Sailing` -> `Sailing`.
    Tag(String),
}

impl Component {
    /// Every kind of level, a tag with an empty root standing for any root.
    pub fn kinds() -> [Component; 6] {
        [
            Component::Country,
            Component::Region,
            Component::City,
            Component::Year,
            Component::Month,
            Component::Tag(String::new()),
        ]
    }

    pub fn title(&self) -> String {
        match self {
            Component::Country => "Country".to_string(),
            Component::Region => "Region".to_string(),
            Component::City => "City".to_string(),
            Component::Year => "Year".to_string(),
            Component::Month => "Month".to_string(),
            Component::Tag(root) if root.is_empty() => "Tag".to_string(),
            Component::Tag(root) => format!("Tag under {root}"),
        }
    }

    fn key(&self) -> String {
        match self {
            Component::Country => "country".to_string(),
            Component::Region => "region".to_string(),
            Component::City => "city".to_string(),
            Component::Year => "year".to_string(),
            Component::Month => "month".to_string(),
            Component::Tag(root) => format!("tag:{root}"),
        }
    }

    fn from_key(key: &str) -> Option<Component> {
        Some(match key {
            "country" => Component::Country,
            "region" => Component::Region,
            "city" => Component::City,
            "year" => Component::Year,
            "month" => Component::Month,
            _ => Component::Tag(key.strip_prefix("tag:")?.to_string()),
        })
    }

    fn shape(&self) -> Shape {
        match self {
            Component::Year => Shape::Year,
            Component::Month => Shape::Month,
            _ => Shape::Name,
        }
    }
}

/// What a folder name looks like, which is all a parser can tell levels apart by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Year,
    Month,
    Name,
}

impl Shape {
    fn of(folder: &str) -> Shape {
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        match folder.split_once('-') {
            None if folder.len() == 4 && digits(folder) => Shape::Year,
            Some((year, month)) if year.len() == 4 && month.len() == 2 && digits(year) && digits(month) => Shape::Month,
            _ => Shape::Name,
        }
    }
}

/// A folder name that is neither a year nor a month, as a country, a city or a tag folder is.
pub fn plain_name(folder: &str) -> bool {
    Shape::of(folder) == Shape::Name
}

/// One level above the event folder. An optional one may be left out, as an event without a city
/// sits right in its country.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Level {
    pub component: Component,
    pub optional: bool,
}

/// The folder levels above the event folder, in order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Layout {
    pub levels: Vec<Level>,
}

/// The layouts offered by name. The first is the default.
pub const PRESETS: [(&str, &str); 6] = [
    ("Country / City / Event", "country/city?"),
    ("Country / Event", "country"),
    ("Year / Event", "year"),
    ("Year / Country / Event", "year/country"),
    ("Country / Year / Event", "country/year"),
    ("Event Only", ""),
];

impl Default for Layout {
    fn default() -> Layout {
        Layout::read(PRESETS[0].1).expect("the default layout reads")
    }
}

impl fmt::Display for Layout {
    /// The text it is kept as: `country/city?`, `year/tag:topics`, or nothing for events only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let levels: Vec<String> = self
            .levels
            .iter()
            .map(|level| format!("{}{}", level.component.key(), if level.optional { "?" } else { "" }))
            .collect();
        write!(f, "{}", levels.join("/"))
    }
}

impl Layout {
    /// A layout from the text it is kept as, checked.
    pub fn read(text: &str) -> Result<Layout, String> {
        let levels = text
            .split('/')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (key, optional) = match part.strip_suffix('?') {
                    Some(key) => (key, true),
                    None => (part, false),
                };
                Component::from_key(key)
                    .map(|component| Level { component, optional })
                    .ok_or_else(|| format!("{key} is not a folder level"))
            })
            .collect::<Result<Vec<Level>, String>>()?;
        let layout = Layout { levels };
        layout.check()?;
        Ok(layout)
    }

    /// The preset this layout is, by its name.
    pub fn preset(&self) -> Option<&'static str> {
        let text = self.to_string();
        PRESETS
            .iter()
            .find(|(_, preset)| *preset == text)
            .map(|(name, _)| *name)
    }

    /// `Country / City / Event`, the optional levels in brackets.
    pub fn title(&self) -> String {
        let mut parts: Vec<String> = self
            .levels
            .iter()
            .map(|level| match level.optional {
                true => format!("({})", level.component.title()),
                false => level.component.title(),
            })
            .collect();
        parts.push("Event".to_string());
        parts.join(" / ")
    }

    /// Where an event would lie in this layout, from what its folders say now: the levels it
    /// cannot know yet in angle brackets, `Germany/<Region>/2019-07-13 Party`.
    pub fn example(&self, placement: &Placement) -> String {
        let date = placement.event_text.as_deref().unwrap_or("0000-00-00");
        let mut folders: Vec<String> = self
            .levels
            .iter()
            .map(|level| {
                let known = match &level.component {
                    Component::Year => date.get(..4).map(String::from),
                    Component::Month => date.get(..7).map(String::from),
                    component => placement
                        .levels
                        .iter()
                        .find(|(found, _)| found == component)
                        .map(|(_, folder)| folder.clone()),
                };
                known.unwrap_or_else(|| format!("<{}>", level.component.title()))
            })
            .collect();
        folders.push(placement.event_folder().unwrap_or_else(|| date.to_string()));
        folders.join("/")
    }

    pub fn has(&self, component: &Component) -> bool {
        self.levels.iter().any(|level| &level.component == component)
    }

    /// Why a layout cannot be used: a level twice, a tag without a root, or levels that could be
    /// read from one path in two ways.
    pub fn check(&self) -> Result<(), String> {
        for (at, level) in self.levels.iter().enumerate() {
            if let Component::Tag(root) = &level.component
                && (root.trim().is_empty() || root.contains('/') || root.trim() != root)
            {
                return Err("a tag level needs the name of a top-level tag as its root".to_string());
            }
            if self.levels[..at]
                .iter()
                .any(|before| before.component == level.component)
            {
                return Err(format!("{} is there twice", level.component.title()));
            }
        }
        let optional: Vec<usize> = (0..self.levels.len()).filter(|at| self.levels[*at].optional).collect();
        let mut seen: Vec<(Vec<Shape>, u32)> = Vec::new();
        for left_out in 0u32..(1 << optional.len()) {
            let shapes: Vec<Shape> = (0..self.levels.len())
                .filter(|at| {
                    optional
                        .iter()
                        .position(|index| index == at)
                        .is_none_or(|bit| left_out & (1 << bit) == 0)
                })
                .map(|at| self.levels[at].component.shape())
                .collect();
            if let Some((_, other)) = seen.iter().find(|(before, _)| *before == shapes) {
                let names = |mask: u32| -> Vec<String> {
                    optional
                        .iter()
                        .enumerate()
                        .filter(|(bit, _)| mask & (1 << bit) != 0)
                        .map(|(_, at)| self.levels[*at].component.title())
                        .collect()
                };
                let mut both = names(left_out ^ other);
                both.dedup();
                return Err(format!(
                    "{} cannot be told apart when one is left out",
                    both.join(" and ")
                ));
            }
            seen.push((shapes, left_out));
        }
        Ok(())
    }

    /// The folders above an event read as this layout's levels, or `None` when they are not.
    /// `event` is the date part of the event folder the year and month levels must agree with.
    fn place(&self, folders: &[&str], event: Option<&str>) -> Option<Vec<(Component, String)>> {
        let mut found = Vec::new();
        match self.walk(0, folders, event, &mut found) {
            true => Some(found),
            false => None,
        }
    }

    fn walk(&self, at: usize, folders: &[&str], event: Option<&str>, found: &mut Vec<(Component, String)>) -> bool {
        let Some(level) = self.levels.get(at) else {
            return folders.is_empty();
        };
        if let Some(folder) = folders.first()
            && Shape::of(folder) == level.component.shape()
            && agrees(&level.component, folder, event)
        {
            found.push((level.component.clone(), folder.to_string()));
            if self.walk(at + 1, &folders[1..], event, found) {
                return true;
            }
            found.pop();
        }
        level.optional && self.walk(at + 1, folders, event, found)
    }

    /// Whether folders holding no event are a start of this layout that only ever needs more
    /// levels to come: `China/` under `Country/City`, `2019/` under `Year/Country`.
    fn holds_loose(&self, folders: &[&str]) -> Option<Vec<(Component, String)>> {
        let required: Vec<&Level> = self.levels.iter().filter(|level| !level.optional).collect();
        if folders.is_empty() || folders.len() > required.len() {
            return None;
        }
        folders
            .iter()
            .zip(&required)
            .map(|(folder, level)| {
                (Shape::of(folder) == level.component.shape()).then(|| (level.component.clone(), folder.to_string()))
            })
            .collect()
    }
}

/// A year or month level has to say the event's own year or month.
fn agrees(component: &Component, folder: &str, event: Option<&str>) -> bool {
    match (component, event) {
        (Component::Year, Some(date)) => date.get(..4) == Some(folder),
        (Component::Month, Some(date)) => date.get(..7) == Some(folder),
        _ => true,
    }
}

/// How well a path fits the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// An event with a date under the layout's levels, maybe in sub-folders.
    InLayout,
    /// An event with a date, but the folders above it are not the layout's.
    OffLayout,
    /// In a folder of the layout's levels, without an event.
    LooseInFolder,
    /// Directly in the library root.
    #[default]
    LooseAtRoot,
    /// There is a folder where the event should be, but its name carries no date.
    EventWithoutDate,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::InLayout => "in the layout",
            Fit::OffLayout => "off the layout",
            Fit::LooseInFolder => "loose in a folder",
            Fit::LooseAtRoot => "loose at root",
            Fit::EventWithoutDate => "event without a date",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    /// The tag level's folder, when the layout has one.
    pub tag: Option<String>,
    pub event_text: Option<String>,
    pub event_year: Option<i64>,
    pub event_month: Option<i64>,
    pub event_day: Option<i64>,
    pub event_name: Option<String>,
    pub sub_path: Option<String>,
    /// The event's folder inside the library, `<levels>/YYYY-MM-DD Event`.
    pub event_dir: Option<String>,
    /// The folders above the event, or above a loose photo, as they are.
    pub above: Vec<String>,
    /// The layout's levels those folders were read as, when they were.
    pub levels: Vec<(Component, String)>,
    pub fit: Fit,
}

struct Event {
    text: String,
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    name: Option<String>,
}

impl Placement {
    pub fn parse(rel_path: &str, layout: &Layout) -> Placement {
        let parts: Vec<&str> = rel_path.split('/').filter(|part| !part.is_empty()).collect();
        let folders = &parts[..parts.len().saturating_sub(1)];
        if folders.is_empty() {
            return Placement::default();
        }

        let Some((at, event)) = folders
            .iter()
            .enumerate()
            .find_map(|(at, folder)| parse_event(folder).map(|event| (at, event)))
        else {
            if let Some(levels) = layout.holds_loose(folders) {
                return Placement {
                    above: folders.iter().map(|folder| folder.to_string()).collect(),
                    fit: Fit::LooseInFolder,
                    ..Placement::default()
                }
                .with(levels);
            }
            let first = layout.holds_loose(&folders[..1]).unwrap_or_default();
            return Placement {
                sub_path: Some(folders[1..].join("/")).filter(|sub| !sub.is_empty()),
                above: folders.iter().map(|folder| folder.to_string()).collect(),
                fit: Fit::EventWithoutDate,
                ..Placement::default()
            }
            .with(first);
        };

        let above = &folders[..at];
        let sub = &folders[at + 1..];
        let levels = layout.place(above, Some(&event.text));
        Placement {
            event_text: Some(event.text),
            event_year: event.year,
            event_month: event.month,
            event_day: event.day,
            event_name: event.name,
            sub_path: (!sub.is_empty()).then(|| sub.join("/")),
            event_dir: Some(folders[..=at].join("/")),
            above: above.iter().map(|folder| folder.to_string()).collect(),
            fit: match levels {
                Some(_) => Fit::InLayout,
                None => Fit::OffLayout,
            },
            ..Placement::default()
        }
        .with(levels.unwrap_or_default())
    }

    /// What an event folder alone says: `Country/City/2019-07-13 Party`.
    pub fn of_folder(event_dir: &str, layout: &Layout) -> Placement {
        Placement::parse(&format!("{event_dir}/-"), layout)
    }

    fn with(mut self, levels: Vec<(Component, String)>) -> Placement {
        for (component, folder) in &levels {
            let folder = Some(folder.clone());
            match component {
                Component::Country => self.country = folder,
                Component::Region => self.region = folder,
                Component::City => self.city = folder,
                Component::Tag(_) => self.tag = folder,
                Component::Year | Component::Month => {}
            }
        }
        self.levels = levels;
        self
    }

    /// In the layout, with every level it has - no optional one left out.
    pub fn complete(&self, layout: &Layout) -> bool {
        self.fits() && self.levels.len() == layout.levels.len()
    }

    pub fn fits(&self) -> bool {
        self.fit == Fit::InLayout
    }

    /// The folder name of the event itself: `2019-07-13 Party`.
    pub fn event_folder(&self) -> Option<String> {
        let date = self.event_text.as_deref()?;
        Some(match self.event_name.as_deref() {
            Some(name) => format!("{date} {name}"),
            None => date.to_string(),
        })
    }
}

/// `YYYY-MM-DD Name`, where any part of the date may be zeroed out.
fn parse_event(folder: &str) -> Option<Event> {
    let (date, name) = folder.split_at_checked(10)?;
    if !name.is_empty() && !name.starts_with(' ') {
        return None;
    }
    let digits: Vec<&str> = date.split('-').collect();
    if digits.len() != 3 || digits[0].len() != 4 || digits[1].len() != 2 || digits[2].len() != 2 {
        return None;
    }
    let numbers: Vec<i64> = digits.iter().map(|part| part.parse().ok()).collect::<Option<_>>()?;
    let name = name.trim();

    Some(Event {
        text: date.to_string(),
        year: (numbers[0] > 0).then_some(numbers[0]),
        month: (numbers[1] > 0).then_some(numbers[1]),
        day: (numbers[2] > 0).then_some(numbers[2]),
        name: (!name.is_empty()).then(|| name.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(path: &str) -> Placement {
        Placement::parse(path, &Layout::default())
    }

    fn under(text: &str, path: &str) -> Placement {
        Placement::parse(path, &Layout::read(text).unwrap())
    }

    #[test]
    fn reads_a_full_event() {
        let placement = parse("Germany/2019-07-13 Sommerfest/img_0657.jpg");
        assert_eq!(placement.country.as_deref(), Some("Germany"));
        assert_eq!(placement.city, None);
        assert_eq!(placement.event_text.as_deref(), Some("2019-07-13"));
        assert_eq!(
            (placement.event_year, placement.event_month, placement.event_day),
            (Some(2019), Some(7), Some(13))
        );
        assert_eq!(placement.event_name.as_deref(), Some("Sommerfest"));
        assert_eq!(placement.sub_path, None);
        assert!(placement.fits());
    }

    #[test]
    fn keeps_the_holes_in_a_date() {
        let month_only = parse("Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG");
        assert_eq!(
            (month_only.event_year, month_only.event_month, month_only.event_day),
            (Some(2018), Some(10), None)
        );
        assert_eq!(month_only.event_text.as_deref(), Some("2018-10-00"));

        let year_only = parse("China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG");
        assert_eq!(year_only.event_month, Some(1));
        assert_eq!(year_only.event_day, None);

        let unknown = parse("Greece/0000-00-00 Aeron ilands/IMG_0004.JPG");
        assert_eq!(
            (unknown.event_year, unknown.event_month, unknown.event_day),
            (None, None, None)
        );
        assert_eq!(unknown.event_name.as_deref(), Some("Aeron ilands"));
        assert!(unknown.fits(), "a date of zeros is still the convention");
    }

    #[test]
    fn collects_sub_folders() {
        let day = parse("China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG");
        assert_eq!(day.event_name.as_deref(), Some("Besuch Ben"));
        assert_eq!(day.sub_path.as_deref(), Some("2006-08-21"));
        assert_eq!(day.event_dir.as_deref(), Some("China/2006-09-00 Besuch Ben"));

        let photographer = parse("Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG");
        assert_eq!(photographer.sub_path.as_deref(), Some("Kira"));

        let deeper = parse("China/2008-01-00 Tour/2008-01-23 (Guilin)/river/IMG.JPG");
        assert_eq!(deeper.sub_path.as_deref(), Some("2008-01-23 (Guilin)/river"));
    }

    #[test]
    fn recognises_the_city_level_we_are_migrating_to() {
        let placement = parse("Denmark/Copenhagen/2018-10-06 Wedding/DSCF0001.JPG");
        assert_eq!(placement.city.as_deref(), Some("Copenhagen"));
        assert_eq!(placement.event_name.as_deref(), Some("Wedding"));
        assert_eq!(
            placement.event_dir.as_deref(),
            Some("Denmark/Copenhagen/2018-10-06 Wedding")
        );
        assert!(placement.fits());
    }

    #[test]
    fn reports_what_does_not_fit() {
        let loose = parse("China/IMG_3140.JPG");
        assert_eq!(loose.fit, Fit::LooseInFolder);
        assert_eq!(loose.country.as_deref(), Some("China"));
        assert_eq!(loose.event_dir, None);
        assert!(!loose.fits());

        let root = parse("IMG_3140.JPG");
        assert_eq!(root.fit, Fit::LooseAtRoot);
        assert_eq!(root.country, None);

        let no_date = parse("China/Holiday/IMG_0001.JPG");
        assert_eq!(no_date.fit, Fit::EventWithoutDate);
        assert_eq!(no_date.sub_path.as_deref(), Some("Holiday"));

        let almost = parse("China/2006-9-0 Besuch/IMG.JPG");
        assert_eq!(almost.fit, Fit::EventWithoutDate, "a sloppy date is not a date");
    }

    #[test]
    fn takes_names_as_they_come() {
        let spaces = parse("Germany/2019-07-13 Besuch Nora & Anna/img 0657.jpg");
        assert_eq!(spaces.event_name.as_deref(), Some("Besuch Nora & Anna"));

        let unicode = parse("China/2006-09-14 北京/照片.JPG");
        assert_eq!(unicode.event_name.as_deref(), Some("北京"));

        let bare_date = parse("China/2006-09-14/IMG.JPG");
        assert!(bare_date.fits());
        assert_eq!(bare_date.event_name, None);
    }

    #[test]
    fn a_layout_is_kept_as_text_and_read_back() {
        for (_, text) in PRESETS {
            let layout = Layout::read(text).unwrap();
            assert_eq!(layout.to_string(), text);
            assert!(layout.preset().is_some());
        }
        let own = Layout::read("region/tag:topics?/month").unwrap();
        assert_eq!(own.to_string(), "region/tag:topics?/month");
        assert_eq!(own.title(), "Region / (Tag under topics) / Month / Event");
        assert_eq!(own.preset(), None);
        assert_eq!(Layout::default().title(), "Country / (City) / Event");
        assert!(Layout::read("country/planet").is_err());
    }

    #[test]
    fn refuses_a_layout_that_could_be_read_two_ways() {
        assert!(
            Layout::read("country?/city?").is_err(),
            "one folder: a country or a city?"
        );
        assert!(
            Layout::read("country?/city").is_ok(),
            "one folder is the city, two are both"
        );
        assert!(Layout::read("country/city/country").is_err(), "twice");
        assert!(Layout::read("tag:").is_err(), "a tag needs a root");
        assert!(Layout::read("tag:a/b").is_err());
        assert!(Layout::read("year?/country?").is_ok(), "a year looks like a year");
        assert!(Layout::read("year?/month?/country").is_ok());
        assert!(Layout::read("country/city?").is_ok());
        assert!(Layout::read("country/region?/city?").is_err());
        let why = Layout::read("country/region?/city?").unwrap_err();
        assert_eq!(why, "Region and City cannot be told apart when one is left out");
    }

    #[test]
    fn reads_a_library_by_year_and_country() {
        let layout = "year/country";
        let placement = under(layout, "2019/Germany/2019-07-13 Party/sub/IMG.JPG");
        assert!(placement.fits());
        assert_eq!(placement.country.as_deref(), Some("Germany"));
        assert_eq!(placement.event_dir.as_deref(), Some("2019/Germany/2019-07-13 Party"));
        assert_eq!(placement.sub_path.as_deref(), Some("sub"));

        let wrong_year = under(layout, "2018/Germany/2019-07-13 Party/IMG.JPG");
        assert_eq!(
            wrong_year.fit,
            Fit::OffLayout,
            "the year folder has to say the event's year"
        );

        let today = under(layout, "Germany/2019-07-13 Party/IMG.JPG");
        assert_eq!(today.fit, Fit::OffLayout);
        assert_eq!(
            today.event_name.as_deref(),
            Some("Party"),
            "the event is still an event"
        );
        assert_eq!(today.above, vec!["Germany".to_string()]);
        assert_eq!(today.country, None);

        let back = parse("2019/Germany/2019-07-13 Party/IMG.JPG");
        assert_eq!(back.fit, Fit::OffLayout, "and a year library is off the default");
        assert!(parse("Germany/2019-07-13 Party/IMG.JPG").fits());

        assert_eq!(under(layout, "2019/IMG.JPG").fit, Fit::LooseInFolder);
        assert_eq!(under(layout, "2019/Germany/IMG.JPG").fit, Fit::LooseInFolder);
        assert_eq!(under(layout, "Germany/IMG.JPG").fit, Fit::EventWithoutDate);
    }

    #[test]
    fn a_missing_optional_level_still_fits() {
        let layout = "year?/month?/country";
        assert!(under(layout, "Germany/2019-07-13 Party/IMG.JPG").fits());
        assert!(under(layout, "2019-07/Germany/2019-07-13 Party/IMG.JPG").fits());
        assert!(under(layout, "2019/2019-07/Germany/2019-07-13 Party/IMG.JPG").fits());
        assert!(!under(layout, "2019/2019-08/Germany/2019-07-13 Party/IMG.JPG").fits());

        let tagged = under("tag:topics/country?", "Sailing/2019-07-13 Regatta/IMG.JPG");
        assert!(tagged.fits());
        assert_eq!(tagged.tag.as_deref(), Some("Sailing"));
        assert_eq!(tagged.country, None);
    }

    #[test]
    fn an_example_says_what_it_cannot_know() {
        let now = parse("Germany/Hamburg/2019-07-13 Party/IMG.JPG");
        let example = |text: &str| Layout::read(text).unwrap().example(&now);
        assert_eq!(example("year/country"), "2019/Germany/2019-07-13 Party");
        assert_eq!(
            example("country/region/city?"),
            "Germany/<Region>/Hamburg/2019-07-13 Party"
        );
        assert_eq!(
            example("month/tag:topics"),
            "2019-07/<Tag under topics>/2019-07-13 Party"
        );
        assert_eq!(example(""), "2019-07-13 Party");
    }

    #[test]
    fn events_only() {
        let layout = "";
        assert!(under(layout, "2019-07-13 Party/IMG.JPG").fits());
        assert_eq!(under(layout, "Germany/2019-07-13 Party/IMG.JPG").fit, Fit::OffLayout);
        assert_eq!(under(layout, "Germany/IMG.JPG").fit, Fit::EventWithoutDate);
        assert_eq!(under(layout, "IMG.JPG").fit, Fit::LooseAtRoot);
    }
}
