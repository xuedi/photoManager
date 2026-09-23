//! What the folders say about a photo: `Country/[City/]YYYY-MM-DD Event/[sub/]file.jpg`.
//! A date may have holes (`2006-09-00`), and what does not fit is reported, never guessed.

/// How well a path fits the convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Country, an event with a date, optionally a city and sub-folders.
    Convention,
    /// Directly in a country folder, without an event.
    LooseInCountry,
    /// Directly in the library root.
    #[default]
    LooseAtRoot,
    /// There is a folder where the event should be, but its name carries no date.
    EventWithoutDate,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Convention => "convention",
            Fit::LooseInCountry => "loose in country",
            Fit::LooseAtRoot => "loose at root",
            Fit::EventWithoutDate => "event without a date",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    pub country: Option<String>,
    pub city: Option<String>,
    pub event_text: Option<String>,
    pub event_year: Option<i64>,
    pub event_month: Option<i64>,
    pub event_day: Option<i64>,
    pub event_name: Option<String>,
    pub sub_path: Option<String>,
    /// The event's folder inside the library, `Country/[City/]YYYY-MM-DD Event`.
    pub event_dir: Option<String>,
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
    pub fn parse(rel_path: &str) -> Placement {
        let parts: Vec<&str> = rel_path.split('/').filter(|part| !part.is_empty()).collect();
        let folders = &parts[..parts.len().saturating_sub(1)];

        let Some((country, rest)) = folders.split_first() else {
            return Placement {
                fit: Fit::LooseAtRoot,
                ..Placement::default()
            };
        };
        let country = Some((*country).to_string());
        if rest.is_empty() {
            return Placement {
                country,
                fit: Fit::LooseInCountry,
                ..Placement::default()
            };
        }

        let (city, event, sub) = match parse_event(rest[0]) {
            Some(event) => (None, Some(event), &rest[1..]),
            None => match rest.get(1).and_then(|name| parse_event(name)) {
                Some(event) => (Some(rest[0].to_string()), Some(event), &rest[2..]),
                None => (None, None, &rest[1..]),
            },
        };
        let event_dir = event.is_some().then(|| folders[..folders.len() - sub.len()].join("/"));

        let sub_path = (!sub.is_empty()).then(|| sub.join("/"));
        match event {
            Some(event) => Placement {
                country,
                city,
                event_text: Some(event.text),
                event_year: event.year,
                event_month: event.month,
                event_day: event.day,
                event_name: event.name,
                sub_path,
                event_dir,
                fit: Fit::Convention,
            },
            None => Placement {
                country,
                sub_path: Some(rest.join("/")),
                fit: Fit::EventWithoutDate,
                ..Placement::default()
            },
        }
    }

    pub fn fits(&self) -> bool {
        self.fit == Fit::Convention
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
        Placement::parse(path)
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
        assert_eq!(loose.fit, Fit::LooseInCountry);
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
}
