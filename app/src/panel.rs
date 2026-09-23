//! What the cache knows about one photo, grouped the way a person asks about it: when, where,
//! what, who, with what, and the file itself, then every raw field. Nothing here opens the file,
//! so the panel is as fresh as the last scan and says so.
//!
//! The coordinates and the place near them come from the local place data. The map is the one
//! thing that asks a server for anything, so it is only made when it is asked for.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use photomanager_core::details::{Details, position};
use photomanager_core::geo::reverse::At;

/// The setting that shows the map without asking every time.
pub const ALWAYS_MAP: &str = "photo-always-map";

/// The panel's groups, rebuilt for every photo.
#[derive(Debug, Default)]
pub struct Panel {
    groups: Vec<gtk::Widget>,
    raw: Option<gtk::ListBox>,
    search: Option<gtk::SearchEntry>,
    map: Option<shumate::SimpleMap>,
    map_slot: Option<gtk::Box>,
    always: Option<adw::SwitchRow>,
    texts: Vec<String>,
}

/// What the panel is filled with.
pub struct Look<'a> {
    pub details: &'a Details,
    pub nearest: Option<&'a At>,
    /// Whether the place data is there to ask at all.
    pub has_places: bool,
    pub always_map: bool,
}

impl Panel {
    pub fn clear(&mut self, root: &gtk::Box) {
        for group in self.groups.drain(..) {
            root.remove(&group);
        }
        self.raw = None;
        self.search = None;
        self.map = None;
        self.map_slot = None;
        self.always = None;
        self.texts.clear();
    }

    /// Shows a photo that the cache does not know, or nothing at all.
    pub fn fill_unknown(&mut self, root: &gtk::Box) {
        self.clear(root);
        let page = adw::StatusPage::builder()
            .icon_name("image-missing-symbolic")
            .title("Not Scanned Yet")
            .description("The cache knows nothing about this photo. Scan the library to read it.")
            .build();
        page.add_css_class("compact");
        self.add(root, page.upcast());
    }

    /// The read-only groups. `edit` stands in for When, Where, Tags and Rating while editing.
    pub fn fill(&mut self, root: &gtk::Box, look: &Look, edit: Option<&gtk::Widget>) {
        self.clear(root);
        let details = look.details;

        let fresh = gtk::Label::builder()
            .label("As the last scan read it")
            .xalign(0.0)
            .build();
        fresh.add_css_class("caption");
        fresh.add_css_class("dim-label");
        self.add(root, fresh.upcast());

        match edit {
            Some(form) => self.add(root, form.clone()),
            None => {
                let when = self.when(details);
                self.add(root, when.upcast());
                let where_ = self.where_(details, look);
                self.add(root, where_.upcast());
                let tags = self.tags(details);
                self.add(root, tags.upcast());
            }
        }
        let people = self.people(details);
        self.add(root, people.upcast());
        let camera = self.camera(details, edit.is_none());
        self.add(root, camera.upcast());
        let file = self.file(details);
        self.add(root, file.upcast());
        let raw = self.raw_fields(details);
        self.add(root, raw.upcast());
    }

    /// Every title and subtitle on the panel, for a test to read.
    pub fn texts(&self) -> Vec<String> {
        self.texts.clone()
    }

    pub fn filter_raw(&self, query: &str) {
        if let Some(search) = &self.search {
            search.set_text(query);
        }
        if let Some(raw) = &self.raw {
            raw.invalidate_filter();
        }
    }

    /// How many raw fields the filter lets through.
    pub fn raw_shown(&self) -> usize {
        let Some(raw) = &self.raw else {
            return 0;
        };
        let mut shown = 0;
        let mut child = raw.first_child();
        while let Some(row) = child {
            if row.is_child_visible() {
                shown += 1;
            }
            child = row.next_sibling();
        }
        shown
    }

    /// The switch that keeps the map on for every photo, when there is a map to show.
    pub fn always_switch(&self) -> Option<adw::SwitchRow> {
        self.always.clone()
    }

    pub fn shows_map(&self) -> bool {
        self.map.is_some()
    }

    /// Makes the map, which asks OpenStreetMap for the tiles around the point.
    pub fn show_map(&mut self, lat: f64, lon: f64) {
        let Some(slot) = &self.map_slot else {
            return;
        };
        if self.map.is_some() {
            return;
        }
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        let map = map_at(lat, lon);
        slot.append(&map);
        self.map = Some(map);
        tracing::info!(lat, lon, "map shown");
    }

    fn add(&mut self, root: &gtk::Box, widget: gtk::Widget) {
        root.append(&widget);
        self.groups.push(widget);
    }

    fn row(&mut self, group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(value)
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        group.add(&row);
        self.texts.push(format!("{title}: {value}"));
        row
    }

    fn note(&mut self, group: &adw::PreferencesGroup, text: &str) {
        let row = adw::ActionRow::builder().title(text).use_markup(false).build();
        row.add_css_class("dim-label");
        group.add(&row);
        self.texts.push(text.to_string());
    }

    fn when(&mut self, details: &Details) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("When").build();
        match &details.taken_at {
            Some(at) => {
                let shown = match &details.taken_offset {
                    Some(offset) => format!("{at} {offset}"),
                    None => at.clone(),
                };
                self.row(&group, "Taken", &shown);
            }
            None => self.note(&group, "No date"),
        }
        if let Some(xmp) = &details.xmp_taken_at
            && Some(xmp) != details.taken_at.as_ref()
        {
            self.row(&group, "XMP date, differs", xmp);
        }
        if let Some(folder) = &details.folder_date {
            let row = self.row(&group, "Folder date", folder);
            if let Some(agrees) = details.agrees {
                let (icon, said) = match agrees {
                    true => ("object-select-symbolic", "agrees"),
                    false => ("dialog-warning-symbolic", "disagrees"),
                };
                let mark = gtk::Image::from_icon_name(icon);
                mark.set_tooltip_text(Some(&format!("The photo's date {said} with the folder")));
                mark.update_property(&[gtk::accessible::Property::Label(said)]);
                row.add_suffix(&mark);
                self.texts.push(format!("Folder date {said}"));
            }
        }
        group
    }

    fn where_(&mut self, details: &Details, look: &Look) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Where").build();
        match details.gps {
            Some((lat, lon)) => {
                self.row(&group, "Coordinates", &position(lat, lon));
                match (look.nearest, look.has_places) {
                    (Some(at), _) => {
                        let (name, far) = nearest(at);
                        let row = self.row(&group, "Nearest place", &name);
                        if let Some(far) = far {
                            let label = gtk::Label::new(Some(&far));
                            label.add_css_class("dim-label");
                            row.add_suffix(&label);
                        }
                    }
                    (None, true) => self.note(&group, "No place nearby"),
                    (None, false) => self.note(&group, "No place data yet: get it on the dashboard"),
                }
            }
            None => self.note(&group, "No coordinates"),
        }

        let place = &details.place;
        let words: Vec<&str> = [&place.location, &place.city, &place.state, &place.country]
            .into_iter()
            .filter_map(|part| part.as_deref())
            .collect();
        match words.is_empty() {
            true => self.note(&group, "No location text"),
            false => {
                self.row(&group, "Location text", &words.join(", "));
            }
        }
        let tags = details.place_tags();
        if !tags.is_empty() {
            self.row(&group, "Place tags", &tags.join("\n"));
        }
        let folder: Vec<&str> = [&details.country, &details.city]
            .into_iter()
            .filter_map(|part| part.as_deref())
            .collect();
        if !folder.is_empty() {
            self.row(&group, "Folder", &folder.join(", "));
        }

        if details.gps.is_some() {
            let slot = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .margin_top(12)
                .build();
            let show = adw::ButtonRow::builder()
                .title("Show Map")
                .start_icon_name("mark-location-symbolic")
                .action_name("win.photo-show-map")
                .build();
            slot.append(&boxed(&show));
            let always = adw::SwitchRow::builder()
                .title("Always Show the Map")
                .subtitle("Asks OpenStreetMap for the area around every photo")
                .active(look.always_map)
                .build();
            let settings = gtk::ListBox::new();
            settings.add_css_class("boxed-list");
            settings.set_selection_mode(gtk::SelectionMode::None);
            settings.set_margin_top(12);
            settings.append(&always);
            group.add(&slot);
            group.add(&settings);
            self.map_slot = Some(slot);
            self.always = Some(always);
        }
        group
    }

    fn tags(&mut self, details: &Details) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Tags").build();
        if details.tags.is_empty() {
            self.note(&group, "No tags");
            return group;
        }
        let mut roots: Vec<(String, Vec<String>)> = Vec::new();
        for tag in &details.tags {
            let (root, rest) = tag.split_once('/').unwrap_or((tag, ""));
            match roots.iter_mut().find(|(known, _)| known == root) {
                Some((_, below)) if !rest.is_empty() => below.push(rest.to_string()),
                Some(_) => {}
                None => {
                    let below = [rest.to_string()].into_iter().filter(|rest| !rest.is_empty());
                    roots.push((root.to_string(), below.collect()));
                }
            }
        }
        for (root, below) in roots {
            self.texts.push(format!("Tag: {root}"));
            if below.is_empty() {
                group.add(&adw::ActionRow::builder().title(&root).use_markup(false).build());
                continue;
            }
            let expander = adw::ExpanderRow::builder()
                .title(glib::markup_escape_text(&root))
                .subtitle(format!("{} below", below.len()))
                .expanded(true)
                .build();
            for path in below {
                let depth = path.matches('/').count();
                let leaf = path.rsplit('/').next().unwrap_or(&path).to_string();
                let row = adw::ActionRow::builder()
                    .title(format!("{}{leaf}", "    ".repeat(depth)))
                    .use_markup(false)
                    .tooltip_text(format!("{root}/{path}"))
                    .build();
                self.texts.push(format!("Tag: {root}/{path}"));
                expander.add_row(&row);
            }
            group.add(&expander);
        }
        group
    }

    fn people(&mut self, details: &Details) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("People").build();
        match details.people.is_empty() {
            true => self.note(&group, "No people"),
            false => {
                for name in &details.people {
                    let row = adw::ActionRow::builder().title(name).use_markup(false).build();
                    row.add_prefix(&gtk::Image::from_icon_name("avatar-default-symbolic"));
                    group.add(&row);
                    self.texts.push(format!("Person: {name}"));
                }
            }
        }
        group
    }

    fn camera(&mut self, details: &Details, rating: bool) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Camera").build();
        let camera: Vec<&str> = [&details.camera_make, &details.camera_model]
            .into_iter()
            .filter_map(|part| part.as_deref())
            .collect();
        match camera.is_empty() {
            true => self.note(&group, "No camera named"),
            false => {
                self.row(&group, "Camera", &camera.join(" "));
            }
        }
        if let (Some(width), Some(height)) = (details.width, details.height) {
            self.row(&group, "Dimensions", &format!("{width} x {height}"));
        }
        if let Some(orientation) = details.orientation {
            self.row(
                &group,
                "Orientation",
                &format!("{orientation}, {}", turned(orientation)),
            );
        }
        if rating {
            let stars = match details.rating {
                Some(stars) => format!("{stars} of 5"),
                None => "none".to_string(),
            };
            self.row(&group, "Rating", &stars);
        }
        group
    }

    fn file(&mut self, details: &Details) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("File").build();
        self.row(&group, "Path", &details.rel_path);
        self.row(&group, "Size", &crate::preview::size(details.size));
        if let Some(content) = &details.content_id {
            self.row(&group, "Content id", content);
        }
        self.row(&group, "Changed (UTC)", &details.changed);
        for (kind, detail) in &details.issues {
            let row = self.row(
                &group,
                "Issue",
                &match detail {
                    Some(detail) => format!("{kind}: {detail}"),
                    None => kind.clone(),
                },
            );
            row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        }
        group
    }

    fn raw_fields(&mut self, details: &Details) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("All Fields")
            .description(format!("{} fields the scan read", details.raw.len()))
            .build();
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filter by name or value")
            .margin_bottom(6)
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Filter the Fields")]);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for (name, value) in &details.raw {
            let row = adw::ActionRow::builder()
                .title(name)
                .subtitle(value)
                .use_markup(false)
                .subtitle_selectable(true)
                .subtitle_lines(4)
                .build();
            row.add_css_class("property");
            list.append(&row);
        }
        let query = Rc::new(RefCell::new(String::new()));
        list.set_filter_func(glib::clone!(
            #[strong]
            query,
            move |row| {
                let query = query.borrow();
                if query.is_empty() {
                    return true;
                }
                let Some(row) = row.downcast_ref::<adw::ActionRow>() else {
                    return true;
                };
                row.title().to_lowercase().contains(query.as_str())
                    || row
                        .subtitle()
                        .is_some_and(|value| value.to_lowercase().contains(query.as_str()))
            }
        ));
        search.connect_changed(glib::clone!(
            #[weak]
            list,
            move |search| {
                *query.borrow_mut() = search.text().trim().to_lowercase();
                list.invalidate_filter();
            }
        ));
        let holder = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        holder.append(&search);
        holder.append(&list);
        group.add(&holder);
        self.raw = Some(list);
        self.search = Some(search);
        group
    }
}

/// The place nearest a point, in words, and how far it is.
pub fn nearest(at: &At) -> (String, Option<String>) {
    match at.places.first() {
        Some(near) => {
            let place = &near.place;
            let mut words = vec![place.name.clone()];
            words.extend(place.area.clone().filter(|area| *area != place.name));
            words.push(place.country_name.clone());
            (words.join(", "), Some(format!("{:.1} km", near.km)))
        }
        None => match &at.country {
            Some((_, name)) => (name.clone(), None),
            None => ("Nowhere in the place data".to_string(), None),
        },
    }
}

fn turned(orientation: i64) -> &'static str {
    match orientation {
        1 => "upright",
        2 => "mirrored",
        3 => "upside down",
        4 => "mirrored upside down",
        5 => "mirrored, turned left",
        6 => "turned right",
        7 => "mirrored, turned right",
        8 => "turned left",
        _ => "unknown",
    }
}

fn boxed(row: &impl IsA<gtk::Widget>) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.append(row);
    list
}

/// OpenStreetMap's standard layer through libshumate, with its attribution, and a mark on the
/// point.
pub fn map_at(lat: f64, lon: f64) -> shumate::SimpleMap {
    let map = shumate::SimpleMap::new();
    let registry = shumate::MapSourceRegistry::with_defaults();
    if let Some(source) = registry.by_id(shumate::MAP_SOURCE_OSM_MAPNIK) {
        map.set_map_source(Some(&source));
    }
    map.set_height_request(260);
    map.set_margin_top(6);
    map.set_overflow(gtk::Overflow::Hidden);
    map.add_css_class("card");
    map.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label("Map")]);
    if let (Some(inner), Some(viewport)) = (map.map(), map.viewport()) {
        let marks = shumate::MarkerLayer::new(&viewport);
        let mark = shumate::Marker::new();
        let pin = gtk::Image::from_icon_name("mark-location-symbolic");
        pin.set_pixel_size(32);
        shumate::prelude::MarkerExt::set_child(&mark, Some(&pin));
        shumate::prelude::LocationExt::set_location(&mark, lat, lon);
        marks.add_marker(&mark);
        map.add_overlay_layer(&marks);
        viewport.set_zoom_level(14.0);
        inner.center_on(lat, lon);
    }
    map
}
