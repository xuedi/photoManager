//! The one rule every date tool writes an offset by. Where a photo was: its position's nearest
//! place, else the country its folder names. The offset is that zone's at the photo's local time,
//! from the IANA rules, so summer time is right, and the hour summer time skips or repeats is
//! refused rather than guessed.

use std::collections::HashMap;

use crate::cache::Dated;
use crate::dates;
use crate::geo::Geo;
use crate::geo::zones::CountryZone;

pub const NO_PLACE_DATA: &str = "there is no place data yet: get it on the dashboard";

/// Which zone a photo was taken in, as far as can be said.
#[derive(Debug, Clone, PartialEq)]
pub enum Zone {
    Known(String),
    /// The folder's country has several zones and the photo no position: someone has to say
    /// which, once for the whole of [`zone_key`].
    Several {
        country: String,
        zones: Vec<(String, i64)>,
    },
    /// Nothing says where it was, and why.
    Unknown(String),
}

/// The zones of the photos a tool looks at, each point and each country asked of the place data
/// once.
pub struct Offsets<'a> {
    geo: Option<&'a Geo>,
    near: HashMap<(i64, i64), Option<String>>,
    countries: HashMap<String, Zone>,
}

impl<'a> Offsets<'a> {
    /// Place data with nothing in it is no place data.
    pub fn new(geo: Option<&'a Geo>) -> Offsets<'a> {
        Offsets {
            geo: geo.filter(|geo| geo.is_filled()),
            near: HashMap::new(),
            countries: HashMap::new(),
        }
    }

    pub fn zone(&mut self, photo: &Dated) -> Zone {
        let Some(geo) = self.geo else {
            return Zone::Unknown(NO_PLACE_DATA.to_string());
        };
        if let Some((lat, lon)) = photo.gps {
            let key = ((lat * 100.0).round() as i64, (lon * 100.0).round() as i64);
            let near = match self.near.get(&key) {
                Some(known) => known.clone(),
                None => {
                    let found = geo.zone_near(lat, lon).unwrap_or_else(|error| {
                        tracing::warn!(%error, "the place data could not be asked for a zone");
                        None
                    });
                    self.near.insert(key, found.clone());
                    found
                }
            };
            if let Some(zone) = near {
                return Zone::Known(zone);
            }
        }
        let Some(country) = photo.country.as_deref() else {
            return Zone::Unknown("it has no position and is in no country folder".to_string());
        };
        if let Some(known) = self.countries.get(country) {
            return known.clone();
        }
        let zone = match geo.country(country) {
            Ok(Some((code, name))) => match geo.country_zone(&code) {
                Ok(CountryZone::One(zone)) => Zone::Known(zone),
                Ok(CountryZone::Several(zones)) => Zone::Several { country: name, zones },
                Ok(CountryZone::Unknown) => Zone::Unknown(format!("the place data knows no place in {name}")),
                Err(error) => Zone::Unknown(error.to_string()),
            },
            Ok(None) => Zone::Unknown(format!("the place data knows no country called {country}")),
            Err(error) => Zone::Unknown(error.to_string()),
        };
        self.countries.insert(country.to_string(), zone.clone());
        zone
    }

    /// The offset of the photo at this local time. `chosen` is the zone someone answered for a
    /// country of several.
    pub fn offset(&mut self, photo: &Dated, at: &str, chosen: Option<&str>) -> Result<String, String> {
        match self.zone(photo) {
            Zone::Known(zone) => dates::offset_in(&zone, at),
            Zone::Several { country, .. } => match chosen {
                Some(zone) => dates::offset_in(zone, at),
                None => Err(format!(
                    "{country} has several time zones: say which on Time Zones and XMP Dates"
                )),
            },
            Zone::Unknown(why) => Err(why),
        }
    }
}

/// What a question about a country of several zones is kept under: the photo's event, or the
/// folder it lies loose in.
pub fn zone_key(photo: &Dated) -> String {
    match &photo.event_dir {
        Some(event_dir) => event_dir.clone(),
        None => photo
            .rel_path
            .rsplit_once('/')
            .map(|(folder, _)| folder.to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn geo(name: &str) -> Geo {
        let dir = std::env::temp_dir().join(format!("photomanager-offsets-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut geo = Geo::open(&dir.join("geo.db")).unwrap();
        let dumps = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
        crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        geo
    }

    fn photo(country: &str, gps: Option<(f64, f64)>) -> Dated {
        Dated {
            rel_path: format!("{country}/2016-06-00 Somewhere/IMG_0001.JPG"),
            country: Some(country.to_string()),
            event_dir: Some(format!("{country}/2016-06-00 Somewhere")),
            gps,
            ..Dated::default()
        }
    }

    #[test]
    fn a_photo_follows_its_position_then_its_folder() {
        let geo = geo("position");
        let mut offsets = Offsets::new(Some(&geo));
        let beijing = photo("China", None);
        assert_eq!(offsets.offset(&beijing, "2012-01-15 12:00:00", None).unwrap(), "+08:00");
        assert_eq!(offsets.offset(&beijing, "2012-07-15 12:00:00", None).unwrap(), "+08:00");

        let hamburg = photo("Germany", None);
        assert_eq!(offsets.offset(&hamburg, "2016-01-11 10:00:00", None).unwrap(), "+01:00");
        assert_eq!(offsets.offset(&hamburg, "2016-07-11 10:00:00", None).unwrap(), "+02:00");
        assert!(
            offsets
                .offset(&hamburg, "2016-03-27 02:30:00", None)
                .unwrap_err()
                .contains("never happened")
        );
        assert!(
            offsets
                .offset(&hamburg, "2016-10-30 02:30:00", None)
                .unwrap_err()
                .contains("twice")
        );

        let in_beijing = photo("Germany", Some((39.9075, 116.39723)));
        assert_eq!(
            offsets.offset(&in_beijing, "2016-07-11 10:00:00", None).unwrap(),
            "+08:00",
            "the position, not the folder"
        );
        let in_copenhagen = photo("Germany", Some((55.68, 12.59)));
        assert_eq!(
            offsets.zone(&in_copenhagen),
            Zone::Known("Europe/Copenhagen".to_string())
        );
    }

    #[test]
    fn a_country_of_several_zones_asks_and_no_place_data_says_so() {
        let geo = geo("several");
        let mut offsets = Offsets::new(Some(&geo));
        let america = photo("United States", None);
        let Zone::Several { country, zones } = offsets.zone(&america) else {
            panic!("the United States has several zones");
        };
        assert_eq!(country, "United States");
        assert!(zones.len() > 1);
        assert!(
            offsets
                .offset(&america, "2016-07-11 10:00:00", None)
                .unwrap_err()
                .contains("several")
        );
        assert_eq!(
            offsets
                .offset(&america, "2016-07-11 10:00:00", Some("America/Chicago"))
                .unwrap(),
            "-05:00"
        );
        let in_new_york = photo("United States", Some((40.71, -74.0)));
        assert_eq!(offsets.zone(&in_new_york), Zone::Known("America/New_York".to_string()));

        assert!(matches!(offsets.zone(&photo("Atlantis", None)), Zone::Unknown(_)));
        let mut none = Offsets::new(None);
        assert_eq!(
            none.offset(&photo("China", None), "2012-01-15 12:00:00", None),
            Err(NO_PLACE_DATA.to_string())
        );
    }
}
