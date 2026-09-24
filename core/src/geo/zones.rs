//! Which time zone a photo was taken in, as far as the place data knows: a point is in the zone
//! of the nearest place, a country in the zone most of its places are in. What the offset of a
//! zone was on a day is the IANA rules' business, in [`crate::dates`].

use rusqlite::params;

use super::reverse::kilometres;
use super::{Geo, Result};

/// How far the search for the nearest place widens, in degrees.
const REACHES: [f64; 4] = [0.25, 1.0, 4.0, 12.0];
/// A second zone holding this share of a country's places makes it a country of several zones.
const SEVERAL: f64 = 0.1;

/// What the place data says about a country's time zone.
#[derive(Debug, Clone, PartialEq)]
pub enum CountryZone {
    One(String),
    /// Its zones, most places first, with how many places each has.
    Several(Vec<(String, i64)>),
    /// The place data has no place in it.
    Unknown,
}

impl Geo {
    /// The zone of the place nearest to the point, if any place is near enough to say.
    pub fn zone_near(&self, lat: f64, lon: f64) -> Result<Option<String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT p.lat, p.lon, p.zone FROM place_at t JOIN place p ON p.id = t.id
             WHERE t.min_lat >= ?1 AND t.max_lat <= ?2 AND t.min_lon >= ?3 AND t.max_lon <= ?4
                AND p.zone IS NOT NULL",
        )?;
        for reach in REACHES {
            let rows = statement.query_map(params![lat - reach, lat + reach, lon - reach, lon + reach], |row| {
                Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?, row.get::<_, String>(2)?))
            })?;
            let mut nearest: Option<(f64, String)> = None;
            for row in rows {
                let (place_lat, place_lon, zone) = row?;
                let km = kilometres(lat, lon, place_lat, place_lon);
                if nearest.as_ref().is_none_or(|(best, _)| km < *best) {
                    nearest = Some((km, zone));
                }
            }
            if let Some((_, zone)) = nearest {
                return Ok(Some(zone));
            }
        }
        Ok(None)
    }

    /// The zones of a country's places, most places first.
    pub fn country_zones(&self, code: &str) -> Result<Vec<(String, i64)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT zone, count(*) FROM place WHERE country = ?1 AND zone IS NOT NULL
             GROUP BY zone ORDER BY count(*) DESC, zone",
        )?;
        let rows = statement.query_map(params![code], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// The zone most of a country's places have, unless another holds a real share of them.
    pub fn country_zone(&self, code: &str) -> Result<CountryZone> {
        let zones = self.country_zones(code)?;
        let total: i64 = zones.iter().map(|(_, count)| count).sum();
        Ok(match zones.as_slice() {
            [] => CountryZone::Unknown,
            [(zone, _)] => CountryZone::One(zone.clone()),
            [(zone, _), (_, second), ..] if (*second as f64) < total as f64 * SEVERAL => CountryZone::One(zone.clone()),
            _ => CountryZone::Several(zones),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn geo() -> Geo {
        let dir = std::env::temp_dir().join("photomanager-geo-zones");
        let _ = std::fs::remove_dir_all(&dir);
        let mut geo = Geo::open(&dir.join("geo.db")).unwrap();
        let dumps = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
        crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        geo
    }

    #[test]
    fn a_point_takes_the_zone_of_the_nearest_place_and_a_country_its_main_one() {
        let geo = geo();
        assert_eq!(geo.zone_near(53.55, 9.99).unwrap().as_deref(), Some("Europe/Berlin"));
        assert_eq!(geo.zone_near(39.90, 116.40).unwrap().as_deref(), Some("Asia/Shanghai"));
        assert_eq!(
            geo.zone_near(55.68, 12.59).unwrap().as_deref(),
            Some("Europe/Copenhagen")
        );
        assert_eq!(geo.zone_near(-60.0, -40.0).unwrap(), None, "the middle of the ocean");

        assert_eq!(
            geo.country_zone("DE").unwrap(),
            CountryZone::One("Europe/Berlin".to_string())
        );
        assert_eq!(
            geo.country_zone("CN").unwrap(),
            CountryZone::One("Asia/Shanghai".to_string())
        );
        assert_eq!(geo.country_zone("XX").unwrap(), CountryZone::Unknown);
        let CountryZone::Several(zones) = geo.country_zone("US").unwrap() else {
            panic!("the United States has several zones");
        };
        assert_eq!(zones[0].0, "America/New_York");
        assert!(zones.iter().any(|(zone, _)| zone == "America/Chicago"));
    }
}
