//! From coordinates to a name. The places come from the R*Tree of points, the country from the
//! country outlines: the nearest city can easily be on the other side of a border.

use rusqlite::params;

use super::lookup::Place;
use super::{Geo, Result};

/// How far the search widens before it gives up, in degrees.
const REACHES: [f64; 4] = [0.25, 1.0, 4.0, 12.0];
const MOST: usize = 5;
/// A part of a town with this many people is a town of its own, such as a city district.
const TOWN: i64 = 100_000;
/// Closer than this a place is where the point is: a position given from a tag stands on it.
const ON_THE_POINT_KM: f64 = 0.01;

#[derive(Debug, Clone, PartialEq)]
pub struct Nearby {
    pub place: Place,
    pub km: f64,
}

/// What is at a point: the country it lies in, the places around it, nearest and biggest
/// first, and the town it is in. Like the forward lookup it decides nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct At {
    pub country: Option<(String, String)>,
    pub places: Vec<Nearby>,
    /// The nearest place inside the country that is a town: a small part of a town, such as a
    /// neighbourhood, only when the point stands on it.
    pub town: Option<Nearby>,
}

impl Geo {
    pub fn at(&self, lat: f64, lon: f64) -> Result<At> {
        let country = self.country_at(lat, lon)?;
        let (places, town) = self.places_around(lat, lon, country.as_ref())?;
        Ok(At { country, places, town })
    }

    /// The places around, from the nearest reach that has any; the town may need a wider one.
    fn places_around(
        &self,
        lat: f64,
        lon: f64,
        country: Option<&(String, String)>,
    ) -> Result<(Vec<Nearby>, Option<Nearby>)> {
        let mut places: Option<Vec<Nearby>> = None;
        for reach in REACHES {
            let mut found = self.within(lat, lon, reach)?;
            if found.is_empty() {
                continue;
            }
            found.sort_by(|one, other| weigh(one).total_cmp(&weigh(other)));
            let town = town(&found, country);
            let places = places.get_or_insert_with(|| found.iter().take(MOST).cloned().collect());
            if town.is_some() {
                return Ok((places.clone(), town));
            }
        }
        Ok((places.unwrap_or_default(), None))
    }

    fn within(&self, lat: f64, lon: f64, reach: f64) -> Result<Vec<Nearby>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT p.id, p.name, p.country, c.name, a.name, p.feature, p.population, p.lat, p.lon
             FROM place_at t
             JOIN place p ON p.id = t.id
             LEFT JOIN country c ON c.code = p.country
             LEFT JOIN area a ON a.code = p.area
             WHERE t.min_lat >= ?1 AND t.max_lat <= ?2 AND t.min_lon >= ?3 AND t.max_lon <= ?4",
        )?;
        let rows = statement.query_map(params![lat - reach, lat + reach, lon - reach, lon + reach], |row| {
            let country: String = row.get(2)?;
            let place = Place {
                id: row.get(0)?,
                name: row.get(1)?,
                country_name: row.get::<_, Option<String>>(3)?.unwrap_or_else(|| country.clone()),
                country,
                area: row.get(4)?,
                feature: row.get(5)?,
                population: row.get(6)?,
                lat: row.get(7)?,
                lon: row.get(8)?,
            };
            let km = kilometres(lat, lon, place.lat, place.lon);
            Ok(Nearby { place, km })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// The country whose outline the point falls in. Only the rings whose box holds the point
    /// are looked at, and a point in a hole - Lesotho inside South Africa - is not inside.
    fn country_at(&self, lat: f64, lon: f64) -> Result<Option<(String, String)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT r.country, r.shape, r.hole, r.points FROM ring_at b JOIN ring r ON r.id = b.id
             WHERE b.min_lat <= ?1 AND b.max_lat >= ?1 AND b.min_lon <= ?2 AND b.max_lon >= ?2",
        )?;
        let rows = statement.query_map(params![lat, lon], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;

        let mut outer: Vec<(String, i64)> = Vec::new();
        let mut holes: Vec<(String, i64)> = Vec::new();
        for row in rows {
            let (country, shape, hole, points) = row?;
            if !contains(&points, lat, lon) {
                continue;
            }
            match hole {
                0 => outer.push((country, shape)),
                _ => holes.push((country, shape)),
            }
        }
        let Some(inside) = outer.into_iter().find(|key| !holes.contains(key)) else {
            return Ok(None);
        };
        let name: String = self
            .connection
            .query_row("SELECT name FROM country WHERE code = ?1", params![inside.0], |row| {
                row.get(0)
            })
            .unwrap_or_else(|_| inside.0.clone());
        Ok(Some((inside.0, name)))
    }
}

/// Of the places around, sorted, the first that is a town, inside the country whose outline
/// holds the point if there is one there: the nearest of all can be over a border.
fn town(found: &[Nearby], country: Option<&(String, String)>) -> Option<Nearby> {
    let is_town = |nearby: &&Nearby| {
        nearby.km < ON_THE_POINT_KM || nearby.place.feature != "PPLX" || nearby.place.population >= TOWN
    };
    let inside = |nearby: &&Nearby| country.is_none_or(|(code, _)| &nearby.place.country == code);
    found
        .iter()
        .filter(is_town)
        .find(inside)
        .or_else(|| found.iter().find(is_town))
        .cloned()
}

/// Nearest wins, but a city people have heard of wins from a little further away.
fn weigh(nearby: &Nearby) -> f64 {
    let size = (nearby.place.population.max(1) as f64).log10() / 7.0;
    nearby.km * (1.0 - 0.6 * size.min(1.0))
}

/// Ray casting over a ring stored as pairs of little-endian f32 longitude and latitude.
fn contains(points: &[u8], lat: f64, lon: f64) -> bool {
    let count = points.len() / 8;
    if count < 3 {
        return false;
    }
    let at = |index: usize| {
        let start = index * 8;
        (
            f32::from_le_bytes(points[start..start + 4].try_into().unwrap()) as f64,
            f32::from_le_bytes(points[start + 4..start + 8].try_into().unwrap()) as f64,
        )
    };

    let mut inside = false;
    let mut previous = at(count - 1);
    for index in 0..count {
        let current = at(index);
        if (current.1 > lat) != (previous.1 > lat) {
            let crossing = (previous.0 - current.0) * (lat - current.1) / (previous.1 - current.1) + current.0;
            if lon < crossing {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

/// Great circle distance, good enough for telling a city from its neighbour.
pub fn kilometres(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    const EARTH: f64 = 6371.0088;
    let (from_lat, to_lat) = (from_lat.to_radians(), to_lat.to_radians());
    let delta_lat = to_lat - from_lat;
    let delta_lon = (to_lon - from_lon).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2) + from_lat.cos() * to_lat.cos() * (delta_lon / 2.0).sin().powi(2);
    2.0 * EARTH * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Once;

    fn geo() -> Geo {
        static IMPORTED: Once = Once::new();
        let file = std::env::temp_dir().join("photomanager-reverse").join("geo.db");
        IMPORTED.call_once(|| {
            let _ = std::fs::remove_dir_all(file.parent().unwrap());
            let mut geo = Geo::open(&file).unwrap();
            let dumps: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
            crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        });
        Geo::open(&file).unwrap()
    }

    #[test]
    fn known_coordinates_land_in_the_right_city_and_country() {
        let geo = geo();
        for (lat, lon, city, country) in [
            (55.6761, 12.5683, "Copenhagen", "DK"),
            (39.9042, 116.4074, "Beijing", "CN"),
            (48.8566, 2.3522, "Paris", "FR"),
            (51.0504, 13.7373, "Dresden", "DE"),
        ] {
            let at = geo.at(lat, lon).unwrap();
            assert_eq!(at.places[0].place.name, city, "the place at {lat},{lon}");
            assert_eq!(
                at.country.as_ref().map(|(code, _)| code.as_str()),
                Some(country),
                "the country at {lat},{lon}"
            );
            assert!(at.places[0].km < 15.0, "{} km away", at.places[0].km);
        }
    }

    #[test]
    fn the_country_comes_from_the_outline_not_from_the_nearest_city() {
        // On the German bank of the Rhine above Rheinfelden: the nearest town is Swiss.
        let at = geo().at(47.5500, 7.7100).unwrap();
        assert_eq!(
            at.country.as_ref().map(|(code, _)| code.as_str()),
            Some("DE"),
            "the point itself is in Germany"
        );
        assert_eq!(at.places[0].place.country, "CH", "the nearest town is across the river");
        assert!(
            at.places.iter().any(|nearby| nearby.place.country == "DE"),
            "the German places around it are offered too"
        );
    }

    #[test]
    fn a_small_part_of_a_town_is_the_town_unless_the_point_stands_on_it() {
        let near_wedding = geo().at(52.5500, 13.3600).unwrap();
        assert_eq!(near_wedding.places[0].place.name, "Wedding", "the nearest place");
        assert_eq!(
            near_wedding.town.unwrap().place.name,
            "Berlin",
            "the town it is part of"
        );

        let on_wedding = geo().at(52.54734, 13.35594).unwrap();
        assert_eq!(
            on_wedding.town.unwrap().place.name,
            "Wedding",
            "a tag's answer stands on it"
        );

        let bergedorf = geo().at(53.4900, 10.2200).unwrap();
        assert_eq!(
            bergedorf.town.unwrap().place.name,
            "Bergedorf",
            "a district the size of a town is one"
        );

        let rhine = geo().at(47.5500, 7.7100).unwrap();
        assert_eq!(
            rhine.town.unwrap().place.country,
            "DE",
            "the town is inside the outline"
        );
    }

    #[test]
    fn a_point_in_no_country_we_know_says_so() {
        let at = geo().at(0.0, -30.0).unwrap();
        assert_eq!(at.country, None, "the middle of the Atlantic");
        assert!(at.places.is_empty());
        assert_eq!(at.town, None);
    }

    #[test]
    fn the_distance_is_measured_the_round_way() {
        assert_eq!(kilometres(0.0, 0.0, 0.0, 0.0), 0.0);
        let hamburg_to_copenhagen = kilometres(53.5511, 9.9937, 55.6761, 12.5683);
        assert!(
            (280.0..300.0).contains(&hamburg_to_copenhagen),
            "{hamburg_to_copenhagen} km"
        );
    }
}
