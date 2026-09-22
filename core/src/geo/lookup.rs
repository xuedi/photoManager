//! From a name to a place. A folder called `places/inChina/Beijing` or an event called
//! `2018-10-00 Wedding Trip to Copenhagen` is turned into candidates with a confidence.
//!
//! It never decides. Whether a candidate is good enough to write into a photo is the tool's
//! business, not this module's.

use rusqlite::params;

use super::{Geo, Result, fold};

/// How sure we are, and what that is worth, is up to the caller: this is only a number.
pub type Confidence = f64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A name of the place, spelled the same once folded.
    Exact,
    /// Every word of the text appears in a name of the place.
    Search,
    /// A name of the place within a typo or two.
    Near,
    /// The name of a region; the candidate is its largest place.
    Area,
}

impl How {
    fn base(self) -> Confidence {
        match self {
            How::Exact => 0.80,
            How::Search => 0.55,
            How::Near => 0.45,
            How::Area => 0.50,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            How::Exact => "exact",
            How::Search => "search",
            How::Near => "near",
            How::Area => "area",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub id: i64,
    pub name: String,
    pub country: String,
    pub country_name: String,
    pub area: Option<String>,
    pub feature: String,
    pub population: i64,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub place: Place,
    pub confidence: Confidence,
    /// The words of the text this came from.
    pub matched: String,
    pub how: How,
}

/// What a piece of text turned out to say.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The words left after the noise and the country were taken out.
    pub tokens: Vec<String>,
    /// The country the text itself named, if it named one.
    pub country: Option<(String, String)>,
    pub candidates: Vec<Candidate>,
}

/// Words that say nothing about which place is meant.
const NOISE: [&str; 16] = [
    "in", "at", "on", "the", "of", "and", "to", "a", "im", "der", "die", "das", "und", "places", "place", "near",
];

const MOST: usize = 10;

impl Geo {
    /// Every place this text could mean, best first. `within` narrows it to one country, given
    /// as a code or a country name.
    pub fn find(&self, text: &str, within: Option<&str>) -> Result<Found> {
        let countries = self.countries()?;
        let mut tokens = tokenise(text);

        let mut country = within.and_then(|hint| match_country(&countries, &fold(hint)));
        // A word that is a country name says where, not what: take it out of the words.
        tokens.retain(|token| match match_country(&countries, token) {
            Some(found) => {
                country.get_or_insert(found);
                false
            }
            None => true,
        });

        let mut candidates: Vec<Candidate> = Vec::new();
        let total = tokens.len().max(1);
        for width in (1..=3.min(tokens.len())).rev() {
            for start in 0..=tokens.len().saturating_sub(width) {
                let phrase = tokens[start..start + width].join(" ");
                if candidates.iter().any(|found| found.matched.contains(&phrase)) {
                    continue;
                }
                for (place, how, own, distance) in self.matches(&phrase)? {
                    let confidence = score(&place, how, own, distance, width, total, country.as_ref());
                    candidates.push(Candidate {
                        place,
                        confidence,
                        matched: phrase.clone(),
                        how,
                    });
                }
            }
        }

        candidates.sort_by(|one, other| {
            other
                .confidence
                .total_cmp(&one.confidence)
                .then(other.place.population.cmp(&one.place.population))
        });
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.place.id));
        candidates.truncate(MOST);

        Ok(Found {
            tokens,
            country,
            candidates,
        })
    }

    /// Exact first, then the search index, then a typo away. Each step only if the one before
    /// found nothing, so a good match is never watered down by a worse one.
    fn matches(&self, phrase: &str) -> Result<Vec<(Place, How, bool, usize)>> {
        let exact = self.by_name(
            "SELECT p.id, p.name, p.country, p.area, p.feature, p.population, p.lat, p.lon, n.own
             FROM name n JOIN place p ON p.id = n.place_id WHERE n.folded = ?1 LIMIT 200",
            params![phrase],
            How::Exact,
            0,
        )?;
        if !exact.is_empty() {
            return Ok(exact);
        }

        if let Some(area) = self.largest_in_area(phrase)? {
            return Ok(vec![area]);
        }

        let query = phrase
            .split_whitespace()
            .map(|word| format!("\"{word}\""))
            .collect::<Vec<_>>()
            .join(" AND ");
        let search = self.by_name(
            "SELECT p.id, p.name, p.country, p.area, p.feature, p.population, p.lat, p.lon, n.own
             FROM name_search s JOIN name n ON n.id = s.rowid JOIN place p ON p.id = n.place_id
             WHERE name_search MATCH ?1 LIMIT 200",
            params![query],
            How::Search,
            0,
        )?;
        if !search.is_empty() {
            return Ok(search);
        }

        self.near(phrase)
    }

    /// Names that start the same and are the same length give or take two, then the real
    /// distance. The index over the folded names makes the first part cheap.
    fn near(&self, phrase: &str) -> Result<Vec<(Place, How, bool, usize)>> {
        let allowed = match phrase.chars().count() {
            0..=3 => return Ok(Vec::new()),
            4..=6 => 1,
            _ => 2,
        };
        let prefix: String = phrase.chars().take(3).collect();
        let length = phrase.chars().count();
        let rough = self.by_name(
            "SELECT p.id, p.name, p.country, p.area, p.feature, p.population, p.lat, p.lon, n.own, n.folded
             FROM name n JOIN place p ON p.id = n.place_id
             WHERE n.folded GLOB ?1 AND length(n.folded) BETWEEN ?2 AND ?3 LIMIT 500",
            params![
                format!("{prefix}*"),
                (length - allowed) as i64,
                (length + allowed) as i64
            ],
            How::Near,
            9,
        )?;

        let mut found = Vec::new();
        for (place, how, own, _) in rough {
            let spelling = self.spelling_of(place.id, phrase)?;
            let distance = spelling.map(|name| distance(phrase, &name)).unwrap_or(usize::MAX);
            if distance <= allowed {
                found.push((place, how, own, distance));
            }
        }
        Ok(found)
    }

    /// The nearest spelling this place has to the phrase, to measure the typo against.
    fn spelling_of(&self, place: i64, phrase: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT folded FROM name WHERE place_id = ?1")?;
        let rows = statement.query_map(params![place], |row| row.get::<_, String>(0))?;
        let mut best: Option<(usize, String)> = None;
        for row in rows {
            let name = row?;
            let measured = distance(phrase, &name);
            if best.as_ref().is_none_or(|(so_far, _)| measured < *so_far) {
                best = Some((measured, name));
            }
        }
        Ok(best.map(|(_, name)| name))
    }

    /// A region is not a point: the honest answer is its largest place, said to be an area match.
    fn largest_in_area(&self, phrase: &str) -> Result<Option<(Place, How, bool, usize)>> {
        let code: Option<String> = self
            .connection
            .query_row(
                "SELECT code FROM area WHERE lower(name) = ?1 LIMIT 1",
                params![phrase],
                |row| row.get(0),
            )
            .ok();
        let Some(code) = code else { return Ok(None) };
        Ok(self
            .by_name(
                "SELECT p.id, p.name, p.country, p.area, p.feature, p.population, p.lat, p.lon, 0
                 FROM place p WHERE p.area = ?1 ORDER BY p.population DESC LIMIT 1",
                params![code],
                How::Area,
                0,
            )?
            .into_iter()
            .next())
    }

    fn by_name(
        &self,
        sql: &str,
        parameters: impl rusqlite::Params,
        how: How,
        distance: usize,
    ) -> Result<Vec<(Place, How, bool, usize)>> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map(parameters, |row| {
            Ok((
                Place {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    country: row.get(2)?,
                    country_name: String::new(),
                    area: row.get(3)?,
                    feature: row.get(4)?,
                    population: row.get(5)?,
                    lat: row.get(6)?,
                    lon: row.get(7)?,
                },
                how,
                row.get::<_, i64>(8)? == 1,
                distance,
            ))
        })?;
        let mut found: Vec<(Place, How, bool, usize)> = rows.collect::<rusqlite::Result<_>>()?;
        for (place, ..) in &mut found {
            self.name_the_place(place)?;
        }
        Ok(found)
    }

    /// The codes a row carries are not what a person reads.
    fn name_the_place(&self, place: &mut Place) -> Result<()> {
        place.country_name = self
            .connection
            .query_row(
                "SELECT name FROM country WHERE code = ?1",
                params![place.country],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| place.country.clone());
        if let Some(code) = place.area.clone() {
            place.area = self
                .connection
                .query_row("SELECT name FROM area WHERE code = ?1", params![code], |row| row.get(0))
                .ok();
        }
        Ok(())
    }

    fn countries(&self) -> Result<Vec<Known>> {
        let mut statement = self.connection.prepare_cached("SELECT code, name FROM country")?;
        let rows = statement.query_map([], |row| {
            let code: String = row.get(0)?;
            let name: String = row.get(1)?;
            Ok(Known {
                code: code.to_uppercase(),
                folded_code: fold(&code),
                folded: fold(&name),
                plain: tokenise(&name).join(" "),
                name,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }
}

/// A country as the text might spell it: its code, its name, and its name without the words
/// that carry nothing, so `inNetherland` still finds The Netherlands.
struct Known {
    code: String,
    folded_code: String,
    folded: String,
    plain: String,
    name: String,
}

/// A country the text named, spelled right or nearly right.
fn match_country(countries: &[Known], token: &str) -> Option<(String, String)> {
    if token.len() < 2 {
        return None;
    }
    let named = |known: &Known| (known.code.clone(), known.name.clone());
    for known in countries {
        if token == known.folded_code || token == known.folded || token == known.plain {
            return Some(named(known));
        }
    }
    if token.chars().count() < 5 {
        return None;
    }
    countries
        .iter()
        .find(|known| distance(token, &known.plain) <= 1)
        .map(named)
}

/// Slashes, spaces and the humps of `inChina` all separate words. Numbers say nothing about a
/// place, and neither do the words in [`NOISE`].
pub fn tokenise(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for piece in text.split(['/', '\\']) {
        for word in split_humps(piece) {
            let folded = fold(&word);
            for part in folded.split_whitespace() {
                if part.chars().all(|character| character.is_numeric()) {
                    continue;
                }
                if NOISE.contains(&part) {
                    continue;
                }
                words.push(part.to_string());
            }
        }
    }
    words
}

/// `BeijingSeaSide` is three words, `IMG` is one.
fn split_humps(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for character in text.chars() {
        if character.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = character.is_lowercase() || character.is_numeric();
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// How sure we are: what matched, how much of the text it covered, and whether the country and
/// the size of the place agree with it.
fn score(
    place: &Place,
    how: How,
    own: bool,
    distance: usize,
    width: usize,
    total: usize,
    country: Option<&(String, String)>,
) -> Confidence {
    let covered = width as f64 / total as f64;
    let mut confidence = how.base() * (0.5 + 0.5 * covered);

    confidence -= 0.10 * distance as f64;
    if own {
        confidence += 0.05;
    }
    match country {
        Some((code, _)) if *code == place.country => confidence += 0.10,
        Some(_) => confidence -= 0.30,
        None => {}
    }
    if place.population > 0 {
        confidence += 0.05 * ((place.population as f64).log10() / 7.0).min(1.0);
    }
    if matches!(place.feature.as_str(), "PPLC" | "PPLA") {
        confidence += 0.05;
    }
    confidence.clamp(0.0, 1.0)
}

/// Levenshtein, one row at a time.
fn distance(one: &str, other: &str) -> usize {
    let one: Vec<char> = one.chars().collect();
    let other: Vec<char> = other.chars().collect();
    if one.is_empty() || other.is_empty() {
        return one.len().max(other.len());
    }
    let mut row: Vec<usize> = (0..=other.len()).collect();
    for (i, left) in one.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in other.iter().enumerate() {
            let cost = usize::from(left != right);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(diagonal + cost);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[other.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Once;

    /// The excerpt is imported once; every test then opens its own connection to it.
    fn geo() -> Geo {
        static IMPORTED: Once = Once::new();
        let file = std::env::temp_dir().join("photomanager-lookup").join("geo.db");
        IMPORTED.call_once(|| {
            let _ = std::fs::remove_dir_all(file.parent().unwrap());
            let mut geo = Geo::open(&file).unwrap();
            let dumps: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
            super::super::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        });
        Geo::open(&file).unwrap()
    }

    fn best(text: &str, within: Option<&str>) -> Candidate {
        geo()
            .find(text, within)
            .unwrap()
            .candidates
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("nothing found for {text}"))
    }

    #[test]
    fn words_come_out_of_folders_and_humps() {
        assert_eq!(tokenise("places/inChina/Beijing"), ["china", "beijing"]);
        assert_eq!(tokenise("BeijingSeaSide"), ["beijing", "sea", "side"]);
        assert_eq!(
            tokenise("2018-10-00 Wedding Trip to Copenhagen"),
            ["wedding", "trip", "copenhagen"]
        );
        assert_eq!(tokenise("Weil am Rhein"), ["weil", "am", "rhein"]);
        assert_eq!(tokenise("inNetherland"), ["netherland"]);
        assert_eq!(tokenise("   "), Vec::<String>::new());
    }

    #[test]
    fn a_plain_city_is_found() {
        let found = best("Copenhagen", None);
        assert_eq!(found.place.name, "Copenhagen");
        assert_eq!(found.place.country, "DK");
        assert_eq!(found.how, How::Exact);
        assert!(found.confidence > 0.8, "{}", found.confidence);
    }

    #[test]
    fn the_country_in_the_text_is_used() {
        let found = geo().find("places/inChina/Beijing", None).unwrap();
        assert_eq!(found.country.as_ref().map(|(code, _)| code.as_str()), Some("CN"));
        assert_eq!(found.tokens, ["beijing"]);
        let best = &found.candidates[0];
        assert_eq!(
            (best.place.name.as_str(), best.place.country.as_str()),
            ("Beijing", "CN")
        );
        assert!(best.confidence > 0.9, "{}", best.confidence);
    }

    #[test]
    fn the_same_name_in_two_countries_is_decided_by_the_hint() {
        assert_eq!(best("Paris", None).place.country, "FR");
        assert_eq!(best("Paris", Some("United States")).place.country, "US");
        assert_eq!(best("Paris", Some("FR")).place.country, "FR");

        let american = best("Paris", Some("United States"));
        assert!(american.place.area.is_some(), "the state is named");
    }

    #[test]
    fn lower_case_and_accents_do_not_matter() {
        assert_eq!(best("athens", None).place.country, "GR");
        assert_eq!(best("ATHENS", None).place.country, "GR");
        assert_eq!(
            best("Athens", Some("United States")).place.area.as_deref(),
            Some("Georgia")
        );
        assert_eq!(best("København", None).place.name, "Copenhagen");
    }

    #[test]
    fn a_typo_stays_a_candidate_but_is_not_certain() {
        let found = best("Dresten", None);
        assert_eq!(found.place.name, "Dresden");
        assert_eq!(found.how, How::Near);
        assert!(
            (0.2..0.6).contains(&found.confidence),
            "a near miss is worth looking at, not applying: {}",
            found.confidence
        );
    }

    #[test]
    fn a_name_that_only_starts_with_a_city_is_not_that_city() {
        let found = best("BeijingSeaSide", None);
        assert_eq!(found.place.name, "Beijing");
        assert!(
            found.confidence < 0.65,
            "one word out of three is not an answer: {}",
            found.confidence
        );
        assert!(
            found.confidence < best("Beijing", None).confidence,
            "and it is less sure than the name on its own"
        );
    }

    #[test]
    fn a_city_inside_an_event_name_is_found() {
        let found = best("2018-10-00 Wedding Trip to Copenhagen", None);
        assert_eq!(found.place.name, "Copenhagen");
        assert!(found.confidence > 0.4, "{}", found.confidence);
    }

    #[test]
    fn a_region_answers_with_its_largest_place() {
        let found = best("Hainan", None);
        assert_eq!(found.how, How::Area);
        assert_eq!(found.place.name, "Haikou");
        assert_eq!(found.place.area.as_deref(), Some("Hainan"));
    }

    #[test]
    fn a_country_alone_names_the_country_and_no_place() {
        let found = geo().find("inNetherland", None).unwrap();
        assert_eq!(
            found.country,
            Some(("NL".to_string(), "The Netherlands".to_string())),
            "a country a letter short is still that country"
        );
        assert!(found.tokens.is_empty());
        assert!(found.candidates.is_empty(), "a country is not a place to point at");
    }

    #[test]
    fn nothing_is_found_for_nothing() {
        assert!(geo().find("", None).unwrap().candidates.is_empty());
        assert!(geo().find("qqzzxx", None).unwrap().candidates.is_empty());
        assert!(geo().find("2018-10-00", None).unwrap().candidates.is_empty());
    }

    #[test]
    fn the_distance_is_the_usual_one() {
        assert_eq!(distance("dresten", "dresden"), 1);
        assert_eq!(distance("athens", "athens"), 0);
        assert_eq!(distance("", "paris"), 5);
        assert_eq!(distance("netherland", "the netherlands"), 5);
    }
}
