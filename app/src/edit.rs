//! The form one photo is edited in: date and offset, position, the place in words, tags and
//! rating. It only holds text; `Details::change_to` decides what that text means, so the form
//! never writes and never decides anything on its own.
//!
//! A position can be typed, pasted as `lat, lon`, picked on a map or looked up by a place name.
//! The place in words can be filled from the nearest place. Both lookups are local; only the map
//! asks a server, and only when Pick on Map is pressed.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use photomanager_core::details::{Details, Edited, parse_position, position};
use photomanager_core::write::Place;

use crate::library::Library;
use crate::panel::{map_at, nearest};

pub const DATE: &str = "Date Taken";
pub const OFFSET: &str = "Offset";
pub const POSITION: &str = "Coordinates";
pub const FIND: &str = "Find a Place";
pub const CITY: &str = "City";
pub const STATE: &str = "State";
pub const COUNTRY: &str = "Country";
pub const CODE: &str = "Country Code";
pub const LOCATION: &str = "Sublocation";
pub const ADD_TAG: &str = "Add a Tag";

const RATINGS: [&str; 7] = ["No Rating", "0", "1", "2", "3", "4", "5"];
const SUGGESTIONS: usize = 6;

pub struct Form {
    root: gtk::Box,
    edited: RefCell<Edited>,
    entries: Vec<(&'static str, adw::EntryRow)>,
    tags: adw::PreferencesGroup,
    tag_rows: RefCell<Vec<gtk::Widget>>,
    suggestions: gtk::ListBox,
    known_tags: RefCell<Vec<String>>,
    rating: adw::ComboRow,
    problem: gtk::Label,
    found: gtk::Label,
    map_slot: gtk::Box,
    map: RefCell<Option<(shumate::SimpleMap, shumate::Marker)>>,
    library: Rc<Library>,
    changed: Box<dyn Fn()>,
    /// A field set from the form itself is not a person typing into it.
    setting: std::cell::Cell<bool>,
}

impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Form").field("edited", &self.edited).finish()
    }
}

impl Form {
    pub fn new(details: &Details, library: Rc<Library>, changed: impl Fn() + 'static) -> Rc<Form> {
        let edited = details.edited();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        let entry = |title: &'static str, text: &str| {
            let row = adw::EntryRow::builder().title(title).text(text).build();
            (title, row)
        };
        let place = edited.place.clone();
        let entries = vec![
            entry(DATE, &edited.taken_at),
            entry(OFFSET, &edited.offset),
            entry(POSITION, &edited.position),
            entry(FIND, ""),
            entry(CITY, place.city.as_deref().unwrap_or_default()),
            entry(STATE, place.state.as_deref().unwrap_or_default()),
            entry(COUNTRY, place.country.as_deref().unwrap_or_default()),
            entry(CODE, place.country_code.as_deref().unwrap_or_default()),
            entry(LOCATION, place.location.as_deref().unwrap_or_default()),
            entry(ADD_TAG, ""),
        ];
        let rating = adw::ComboRow::builder()
            .title("Rating")
            .model(&gtk::StringList::new(&RATINGS))
            .build();
        rating.set_selected(match edited.rating {
            Some(stars) => (stars.clamp(0, 5) + 1) as u32,
            None => 0,
        });
        let problem = gtk::Label::builder().wrap(true).xalign(0.0).visible(false).build();
        problem.add_css_class("error");
        let found = gtk::Label::builder().wrap(true).xalign(0.0).visible(false).build();
        found.add_css_class("dim-label");
        found.add_css_class("caption");

        let form = Rc::new(Form {
            root,
            edited: RefCell::new(edited),
            entries,
            tags: adw::PreferencesGroup::builder().title("Tags").build(),
            tag_rows: RefCell::new(Vec::new()),
            suggestions: gtk::ListBox::new(),
            known_tags: RefCell::new(Vec::new()),
            rating,
            problem,
            found,
            map_slot: gtk::Box::builder().orientation(gtk::Orientation::Vertical).build(),
            map: RefCell::new(None),
            library,
            changed: Box::new(changed),
            setting: std::cell::Cell::new(false),
        });
        form.build();
        form
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn edited(&self) -> Edited {
        self.edited.borrow().clone()
    }

    /// Says what is wrong with the form, or nothing.
    pub fn show_problem(&self, problem: Option<&str>) {
        self.problem.set_label(problem.unwrap_or_default());
        self.problem.set_visible(problem.is_some());
    }

    /// The tags the library already uses, for completing a new one.
    pub fn set_known_tags(&self, tags: Vec<String>) {
        *self.known_tags.borrow_mut() = tags;
    }

    /// Types into a field by its title, the way a person would.
    pub fn set_text(&self, title: &str, text: &str) -> bool {
        match self.entry(title) {
            Some(row) => {
                row.set_text(text);
                true
            }
            None => false,
        }
    }

    pub fn set_rating(&self, rating: Option<i64>) {
        self.rating.set_selected(match rating {
            Some(stars) => (stars.clamp(0, 5) + 1) as u32,
            None => 0,
        });
    }

    pub fn add_tag(self: &Rc<Self>, tag: &str) {
        let tag = tag.trim().trim_matches('/').to_string();
        if tag.is_empty() {
            return;
        }
        {
            let mut edited = self.edited.borrow_mut();
            if edited.tags.contains(&tag) {
                return;
            }
            edited.tags.push(tag);
        }
        if let Some(entry) = self.entry(ADD_TAG) {
            self.setting.set(true);
            entry.set_text("");
            self.setting.set(false);
        }
        self.show_suggestions("");
        self.show_tags();
        (self.changed)();
    }

    pub fn remove_tag(self: &Rc<Self>, tag: &str) {
        self.edited.borrow_mut().tags.retain(|each| each != tag);
        self.show_tags();
        (self.changed)();
    }

    fn entry(&self, title: &str) -> Option<adw::EntryRow> {
        self.entries
            .iter()
            .find(|(name, _)| *name == title)
            .map(|(_, row)| row.clone())
    }

    fn text(&self, title: &str) -> String {
        self.entry(title).map(|row| row.text().to_string()).unwrap_or_default()
    }

    fn build(self: &Rc<Self>) {
        self.root.append(&self.problem);

        let when = adw::PreferencesGroup::builder()
            .title("When")
            .description("YYYY-MM-DD HH:MM:SS, and the offset from UTC like +02:00")
            .build();
        when.add(&self.entry(DATE).expect("made above"));
        when.add(&self.entry(OFFSET).expect("made above"));
        self.root.append(&when);

        let where_ = adw::PreferencesGroup::builder()
            .title("Where")
            .description("Latitude, longitude in decimal degrees")
            .build();
        where_.add(&self.entry(POSITION).expect("made above"));
        let find = self.entry(FIND).expect("made above");
        let look_up = suffix_button(&find, "system-search-symbolic", "Look Up the Place");
        look_up.connect_clicked(glib::clone!(
            #[weak(rename_to = form)]
            self,
            #[weak]
            find,
            move |_| form.find_place(&find.text())
        ));
        find.connect_entry_activated(glib::clone!(
            #[weak(rename_to = form)]
            self,
            move |row| form.find_place(&row.text())
        ));
        where_.add(&find);
        let pick = adw::ButtonRow::builder()
            .title("Pick on Map")
            .start_icon_name("mark-location-symbolic")
            .build();
        pick.connect_activated(glib::clone!(
            #[weak(rename_to = form)]
            self,
            move |_| form.show_map()
        ));
        where_.add(&pick);
        where_.add(&self.found);
        where_.add(&self.map_slot);
        self.root.append(&where_);

        let words = adw::PreferencesGroup::builder().title("Location Text").build();
        for title in [LOCATION, CITY, STATE, COUNTRY, CODE] {
            words.add(&self.entry(title).expect("made above"));
        }
        let fill = adw::ButtonRow::builder()
            .title("Fill from the Nearest Place")
            .start_icon_name("find-location-symbolic")
            .build();
        fill.connect_activated(glib::clone!(
            #[weak(rename_to = form)]
            self,
            move |_| form.fill_from_nearest()
        ));
        words.add(&fill);
        self.root.append(&words);

        let add = self.entry(ADD_TAG).expect("made above");
        let adding = suffix_button(&add, "list-add-symbolic", "Add the Tag");
        adding.connect_clicked(glib::clone!(
            #[weak(rename_to = form)]
            self,
            #[weak]
            add,
            move |_| form.add_tag(&add.text())
        ));
        add.connect_entry_activated(glib::clone!(
            #[weak(rename_to = form)]
            self,
            move |row| form.add_tag(&row.text())
        ));
        self.suggestions.add_css_class("boxed-list");
        self.suggestions.set_selection_mode(gtk::SelectionMode::None);
        self.suggestions.set_margin_top(6);
        self.suggestions.set_visible(false);
        self.show_tags();
        self.root.append(&self.tags);

        let rating = adw::PreferencesGroup::new();
        rating.add(&self.rating);
        self.rating.connect_selected_notify(glib::clone!(
            #[weak(rename_to = form)]
            self,
            move |combo| {
                form.edited.borrow_mut().rating = combo.selected().checked_sub(1).map(i64::from);
                (form.changed)();
            }
        ));
        self.root.append(&rating);

        let review = gtk::Button::builder()
            .label("Review Change")
            .action_name("win.photo-review")
            .halign(gtk::Align::End)
            .build();
        review.add_css_class("suggested-action");
        review.add_css_class("pill");
        self.root.append(&review);

        for (title, row) in &self.entries {
            let title = *title;
            row.connect_changed(glib::clone!(
                #[weak(rename_to = form)]
                self,
                move |row| form.typed(title, &row.text())
            ));
        }
    }

    fn typed(self: &Rc<Self>, title: &str, text: &str) {
        if self.setting.get() {
            return;
        }
        {
            let mut edited = self.edited.borrow_mut();
            let part = |text: &str| Some(text.to_string()).filter(|text| !text.trim().is_empty());
            match title {
                DATE => edited.taken_at = text.to_string(),
                OFFSET => edited.offset = text.to_string(),
                POSITION => edited.position = text.to_string(),
                CITY => edited.place.city = part(text),
                STATE => edited.place.state = part(text),
                COUNTRY => edited.place.country = part(text),
                CODE => edited.place.country_code = part(text),
                LOCATION => edited.place.location = part(text),
                ADD_TAG => {
                    drop(edited);
                    self.show_suggestions(text);
                    return;
                }
                _ => return,
            }
        }
        if title == POSITION {
            self.move_mark();
        }
        (self.changed)();
    }

    fn show_tags(self: &Rc<Self>) {
        for row in self.tag_rows.borrow_mut().drain(..) {
            self.tags.remove(&row);
        }
        let mut tags = self.edited.borrow().tags.clone();
        tags.sort();
        let mut rows: Vec<gtk::Widget> = Vec::new();
        for tag in tags {
            let row = adw::ActionRow::builder().title(&tag).use_markup(false).build();
            let remove = gtk::Button::builder()
                .icon_name("list-remove-symbolic")
                .tooltip_text("Remove the Tag")
                .valign(gtk::Align::Center)
                .build();
            remove.add_css_class("flat");
            remove.update_property(&[gtk::accessible::Property::Label(&format!("Remove {tag}"))]);
            let weak: Weak<Form> = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(form) = weak.upgrade() {
                    form.remove_tag(&tag);
                }
            });
            row.add_suffix(&remove);
            rows.push(row.upcast());
        }
        if rows.is_empty() {
            let none = adw::ActionRow::builder().title("No tags").build();
            none.add_css_class("dim-label");
            rows.push(none.upcast());
        }
        rows.push(self.entry(ADD_TAG).expect("made above").upcast());
        rows.push(self.suggestions.clone().upcast());
        for row in &rows {
            self.tags.add(row);
        }
        *self.tag_rows.borrow_mut() = rows;
    }

    /// Up to a handful of the library's tags that contain what was typed.
    fn show_suggestions(self: &Rc<Self>, typed: &str) {
        while let Some(row) = self.suggestions.first_child() {
            self.suggestions.remove(&row);
        }
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            self.suggestions.set_visible(false);
            return;
        }
        let have = self.edited.borrow().tags.clone();
        let matching: Vec<String> = self
            .known_tags
            .borrow()
            .iter()
            .filter(|tag| tag.to_lowercase().contains(&typed) && !have.contains(tag))
            .take(SUGGESTIONS)
            .cloned()
            .collect();
        for tag in &matching {
            let row = adw::ActionRow::builder().title(tag).use_markup(false).build();
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text("Add the Tag")
                .valign(gtk::Align::Center)
                .build();
            add.add_css_class("flat");
            add.update_property(&[gtk::accessible::Property::Label(&format!("Add {tag}"))]);
            let weak = Rc::downgrade(self);
            let tag = tag.clone();
            add.connect_clicked(move |_| {
                if let Some(form) = weak.upgrade() {
                    form.add_tag(&tag);
                }
            });
            row.add_suffix(&add);
            self.suggestions.append(&row);
        }
        self.suggestions.set_visible(!matching.is_empty());
    }

    /// The best match for a place name, from the local place data, as the new position.
    fn find_place(&self, text: &str) {
        let found = self.library.find_place(text);
        let told = match found.first() {
            Some(best) => {
                let place = &best.place;
                self.set_text(POSITION, &position(place.lat, place.lon));
                let mut words = vec![place.name.clone()];
                words.extend(place.area.clone().filter(|area| *area != place.name));
                words.push(place.country_name.clone());
                format!("Found {}", words.join(", "))
            }
            None if self.library.counts().places == 0 => {
                "There is no place data yet: get it on the dashboard".to_string()
            }
            None => format!("Nothing called {:?} in the place data", text.trim()),
        };
        self.found.set_label(&told);
        self.found.set_visible(true);
    }

    /// The nearest place to the position in the form, in words.
    fn fill_from_nearest(&self) {
        let Ok(Some((lat, lon))) = parse_position(&self.text(POSITION)) else {
            self.found.set_label("Give the photo a position first");
            self.found.set_visible(true);
            return;
        };
        let Some(at) = self.library.nearest(lat, lon) else {
            self.found
                .set_label("There is no place data yet: get it on the dashboard");
            self.found.set_visible(true);
            return;
        };
        let Some(near) = at.places.first() else {
            self.found.set_label("No place nearby");
            self.found.set_visible(true);
            return;
        };
        let place = Place {
            city: Some(near.place.name.clone()),
            state: near.place.area.clone(),
            country: Some(near.place.country_name.clone()),
            country_code: Some(near.place.country.clone()),
            location: self.edited.borrow().place.location.clone(),
        };
        for (title, text) in [
            (CITY, &place.city),
            (STATE, &place.state),
            (COUNTRY, &place.country),
            (CODE, &place.country_code),
        ] {
            self.set_text(title, text.as_deref().unwrap_or_default());
        }
        self.found.set_label(&format!("Filled from {}", nearest(&at).0));
        self.found.set_visible(true);
    }

    /// A map to pick the position on: centred on the position there is, or on the whole world.
    fn show_map(self: &Rc<Self>) {
        if self.map.borrow().is_some() {
            return;
        }
        let (lat, lon, zoom) = match parse_position(&self.text(POSITION)) {
            Ok(Some((lat, lon))) => (lat, lon, 14.0),
            _ => (20.0, 0.0, 2.0),
        };
        let (map, mark) = map_at(lat, lon);
        if let Some(viewport) = map.viewport() {
            viewport.set_zoom_level(zoom);
        }
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_released(move |gesture, _, x, y| {
            let (Some(form), Some(widget)) = (weak.upgrade(), gesture.widget()) else {
                return;
            };
            let Some(map) = widget.downcast_ref::<shumate::SimpleMap>() else {
                return;
            };
            if let Some(viewport) = map.viewport() {
                let (lat, lon) = viewport.widget_coords_to_location(map, x, y);
                form.set_text(POSITION, &position(lat, lon));
            }
        });
        map.add_controller(click);
        let hint = gtk::Label::builder()
            .label("Click the map to set the position")
            .xalign(0.0)
            .margin_top(6)
            .build();
        hint.add_css_class("caption");
        hint.add_css_class("dim-label");
        self.map_slot.append(&hint);
        self.map_slot.append(&map);
        *self.map.borrow_mut() = Some((map, mark));
        tracing::info!("map shown to pick a position");
    }

    fn move_mark(&self) {
        let Ok(Some((lat, lon))) = parse_position(&self.text(POSITION)) else {
            return;
        };
        if let Some((_, mark)) = self.map.borrow().as_ref() {
            shumate::prelude::LocationExt::set_location(mark, lat, lon);
        }
    }
}

fn suffix_button(row: &adw::EntryRow, icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    row.add_suffix(&button);
    button
}
