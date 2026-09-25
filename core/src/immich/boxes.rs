//! A face box from how Immich saw the photo to how the file stores it.
//!
//! Immich measures a face on its preview, which is turned the way the photo is shown. MWG wants
//! the box relative to the stored pixels, before any turn, centred and as a fraction of the
//! picture. So the box is taken as two corners, each corner is turned back by the inverse of the
//! orientation, and the box is made again from them. What reaches past the edge is clamped.

use super::Face;

/// A box as MWG keeps it: centre and size, each a fraction of the stored picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Six places are finer than any pixel of a real photo, and read back the same.
fn rounded(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Where a point shown at `(x, y)` lies in the stored picture, both as fractions.
fn turned_back(orientation: i64, x: f64, y: f64) -> (f64, f64) {
    match orientation {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (y, 1.0 - x),
        7 => (1.0 - y, 1.0 - x),
        8 => (1.0 - y, x),
        _ => (x, y),
    }
}

/// Whether a photo of this orientation is shown on its side, so its width is its height.
pub fn sideways(orientation: Option<i64>) -> bool {
    matches!(orientation, Some(5..=8))
}

/// The face's box in the stored picture, or `None` when nothing of it is inside the picture.
pub fn stored(face: &Face, orientation: Option<i64>) -> Option<Area> {
    if face.image_width <= 0 || face.image_height <= 0 {
        return None;
    }
    let fraction = |value: i64, of: i64| (value as f64 / of as f64).clamp(0.0, 1.0);
    let corners = [
        (
            fraction(face.x1, face.image_width),
            fraction(face.y1, face.image_height),
        ),
        (
            fraction(face.x2, face.image_width),
            fraction(face.y2, face.image_height),
        ),
    ];
    let orientation = orientation.unwrap_or(1);
    let [(ax, ay), (bx, by)] = corners.map(|(x, y)| turned_back(orientation, x, y));
    let (left, right) = (ax.min(bx), ax.max(bx));
    let (top, bottom) = (ay.min(by), ay.max(by));
    let area = Area {
        x: rounded((left + right) / 2.0),
        y: rounded((top + bottom) / 2.0),
        w: rounded(right - left),
        h: rounded(bottom - top),
    };
    (area.w > 0.0 && area.h > 0.0).then_some(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Immich's own `orientRegionInfo` (metadata.service.ts, v3.0.2): a region as the file
    /// stores it, to the region as the photo is shown. Ported line by line, to check the way back
    /// against the way there.
    fn orient_region_info(area: Area, orientation: i64) -> Area {
        let Area {
            mut x,
            mut y,
            mut w,
            mut h,
        } = area;
        let sidewards = [5, 6, 7, 8].contains(&orientation);
        match orientation {
            2 => x = 1.0 - x,
            3 => (x, y) = (1.0 - x, 1.0 - y),
            4 => y = 1.0 - y,
            5 => (x, y) = (y, x),
            6 => (x, y) = (1.0 - y, x),
            7 => (x, y) = (1.0 - y, 1.0 - x),
            8 => (x, y) = (y, 1.0 - x),
            _ => {}
        }
        if sidewards {
            (w, h) = (h, w);
        }
        Area { x, y, w, h }
    }

    /// A box shown at this area, as Immich would hand it over on a preview of this size.
    fn face_at(shown: Area, width: i64, height: i64) -> Face {
        let pixel = |fraction: f64, of: i64| (fraction * of as f64).round() as i64;
        Face {
            id: "f".to_string(),
            asset_id: "a".to_string(),
            person_id: None,
            image_width: width,
            image_height: height,
            x1: pixel(shown.x - shown.w / 2.0, width),
            y1: pixel(shown.y - shown.h / 2.0, height),
            x2: pixel(shown.x + shown.w / 2.0, width),
            y2: pixel(shown.y + shown.h / 2.0, height),
        }
    }

    fn near(one: Area, other: Area) -> bool {
        [(one.x, other.x), (one.y, other.y), (one.w, other.w), (one.h, other.h)]
            .iter()
            .all(|(a, b)| (a - b).abs() < 1e-3)
    }

    #[test]
    fn every_orientation_turns_back_to_what_immich_turned_it_from() {
        let kept = Area {
            x: 0.3,
            y: 0.2,
            w: 0.1,
            h: 0.3,
        };
        for orientation in 1..=8 {
            let shown = orient_region_info(kept, orientation);
            let (width, height) = match sideways(Some(orientation)) {
                true => (2000, 3000),
                false => (3000, 2000),
            };
            let back = stored(&face_at(shown, width, height), Some(orientation)).unwrap();
            assert!(near(back, kept), "orientation {orientation}: {back:?} is not {kept:?}");
        }
        let unknown = stored(&face_at(kept, 3000, 2000), None).unwrap();
        assert!(near(unknown, kept), "no orientation is the stored one");
    }

    #[test]
    fn a_turned_photo_gets_its_box_where_the_face_is_stored() {
        let shown_top_left = Face {
            id: "f".to_string(),
            asset_id: "a".to_string(),
            person_id: None,
            image_width: 160,
            image_height: 240,
            x1: 10,
            y1: 20,
            x2: 50,
            y2: 80,
        };
        let area = stored(&shown_top_left, Some(6)).unwrap();
        assert_eq!(
            area,
            Area {
                x: 0.208333,
                y: 0.8125,
                w: 0.25,
                h: 0.25
            },
            "shown top left, a photo turned clockwise stores it bottom left"
        );
    }

    #[test]
    fn a_box_past_the_edge_is_clamped_and_one_outside_is_nothing() {
        let past = Face {
            id: "f".to_string(),
            asset_id: "a".to_string(),
            person_id: None,
            image_width: 100,
            image_height: 100,
            x1: -20,
            y1: 90,
            x2: 20,
            y2: 130,
        };
        assert_eq!(
            stored(&past, Some(1)),
            Some(Area {
                x: 0.1,
                y: 0.95,
                w: 0.2,
                h: 0.1
            })
        );
        let outside = Face {
            x1: 120,
            x2: 150,
            ..past
        };
        assert_eq!(stored(&outside, Some(1)), None);
    }
}
