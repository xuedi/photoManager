//! Position from a Neighbour: one event as a timeline, a lane per camera, where a measured photo is
//! picked as the source and the photos taken at the same spot are selected by eye and given its
//! position. Nothing is written here: the groups become one change set for the preview.
//!
//! Every step is an action of the `neighbour` group, so what a click does a test can do too.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use photomanager_core::dates;
use photomanager_core::edits::{Edit, Value};
use photomanager_core::tools::neighbour::{Group, Moment, Neighbours, Position, Reach, Timeline};

use crate::library::Library;
use crate::thumbnails::{self, Loader, Request, Slot};
use crate::tools::Tools;

const TILE: i32 = 64;
const GAP: i32 = 6;
/// A lane stacks crowded photos in this many rows at most, then they overlap.
const ROWS: usize = 4;
const START: i32 = 12;
const AXIS: i32 = 28;
/// What 100 px of the axis stand for, from a minute to three days.
const ZOOMS: [i64; 9] = [60, 300, 900, 1800, 3600, 3 * 3600, 12 * 3600, 86_400, 3 * 86_400];
/// The widest a timeline is made on opening before it is zoomed out.
const FITS: i64 = 1400;
/// Tick steps of the axis, the first that leaves room for a label is used.
const TICKS: [i64; 12] = [
    60,
    300,
    900,
    1800,
    3600,
    7200,
    3 * 3600,
    6 * 3600,
    12 * 3600,
    86_400,
    2 * 86_400,
    7 * 86_400,
];
/// How many colours the groups take turns with.
const PALETTE: usize = 5;

/// One photo on the page, and where it was put.
#[derive(Debug)]
pub(crate) struct Tile {
    button: gtk::Button,
    slot: Slot,
    /// Where it sits on the timeline; `None` in the strip of undated photos.
    at: Option<(i32, i32)>,
}

#[derive(Debug)]
pub(crate) struct Widgets {
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    preview: gtk::Button,
    source: adw::ActionRow,
    map: gtk::Button,
    zoom: gtk::Label,
    lanes: adw::PreferencesGroup,
    names: gtk::Box,
    fixed: gtk::Fixed,
    band: gtk::Box,
    undated_title: gtk::Label,
    undated: gtk::FlowBox,
    picked: gtk::Label,
    reach: gtk::DropDown,
    give: gtk::Button,
    groups: adw::PreferencesGroup,
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct NeighbourPage {
        pub library: RefCell<Option<Rc<Library>>>,
        pub loader: RefCell<Option<Loader<gdk::Texture>>>,
        pub event: RefCell<Option<String>>,
        pub timeline: RefCell<Option<Timeline>>,
        pub loading: Cell<bool>,
        pub source: RefCell<Option<String>>,
        pub selected: RefCell<BTreeSet<String>>,
        /// Where a run selected with Shift starts.
        pub anchor: RefCell<Option<String>>,
        pub reach: Cell<Reach>,
        pub groups: RefCell<Vec<Group>>,
        /// How far each camera's photos are moved along the axis, for the eye only.
        pub shifts: RefCell<HashMap<String, i64>>,
        pub zoom: Cell<usize>,
        pub(crate) tiles: RefCell<HashMap<String, Tile>>,
        pub lane_rows: RefCell<Vec<gtk::Widget>>,
        pub group_rows: RefCell<Vec<gtk::Widget>>,
        pub drawn: RefCell<Vec<gtk::Widget>>,
        pub band_from: Cell<Option<(f64, f64)>>,
        pub said: RefCell<String>,
        pub(crate) widgets: OnceCell<Widgets>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NeighbourPage {
        const NAME: &'static str = "PmNeighbour";
        type Type = super::NeighbourPage;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for NeighbourPage {
        fn constructed(&self) {
            self.parent_constructed();
            let page = self.obj();
            page.build();
            page.install_actions();
        }
    }

    impl WidgetImpl for NeighbourPage {}
    impl BinImpl for NeighbourPage {}
}

glib::wrapper! {
    pub struct NeighbourPage(ObjectSubclass<imp::NeighbourPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for NeighbourPage {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl NeighbourPage {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        let imp = self.imp();
        *imp.loader.borrow_mut() = library
            .as_ref()
            .map(|library| thumbnails::textures(library.thumbs().clone()));
        if let Some(library) = &library {
            let page = self.downgrade();
            library.connect_changed(move || {
                if let Some(page) = page.upgrade() {
                    page.reload();
                }
            });
        }
        *imp.library.borrow_mut() = library;
    }

    /// Shows an event's timeline. Another event than the one shown starts afresh.
    pub fn open(&self, event: &str) {
        let imp = self.imp();
        if imp.event.borrow().as_deref() != Some(event) {
            *imp.event.borrow_mut() = Some(event.to_string());
            *imp.timeline.borrow_mut() = None;
            self.forget();
            imp.shifts.borrow_mut().clear();
            imp.reach.set(Reach::default());
            self.widgets().reach.set_selected(reach_index(Reach::default()));
        }
        let name = event.rsplit('/').next().unwrap_or(event);
        self.widgets().title.set_subtitle(name);
        tracing::info!(event, "neighbour timeline opened");
        self.load(true);
    }

    pub fn event(&self) -> Option<String> {
        self.imp().event.borrow().clone()
    }

    /// Whether the timeline of the event is there to look at.
    pub fn is_loaded(&self) -> bool {
        !self.imp().loading.get() && self.imp().timeline.borrow().is_some()
    }

    /// Each lane's camera and how many photos it holds, in order.
    pub fn lanes(&self) -> Vec<(String, usize)> {
        self.imp()
            .timeline
            .borrow()
            .iter()
            .flat_map(|timeline| &timeline.lanes)
            .map(|lane| (lane.camera.clone(), lane.photos.len()))
            .collect()
    }

    pub fn undated(&self) -> Vec<String> {
        self.imp()
            .timeline
            .borrow()
            .iter()
            .flat_map(|timeline| &timeline.undated)
            .map(|photo| photo.rel_path.clone())
            .collect()
    }

    pub fn source(&self) -> Option<String> {
        self.imp().source.borrow().clone()
    }

    pub fn selected(&self) -> Vec<String> {
        self.imp().selected.borrow().iter().cloned().collect()
    }

    pub fn groups(&self) -> Vec<Group> {
        self.imp().groups.borrow().clone()
    }

    /// Where a photo sits along the axis, in pixels.
    pub fn x_of(&self, rel_path: &str) -> Option<i32> {
        self.imp().tiles.borrow().get(rel_path)?.at.map(|(x, _)| x)
    }

    /// What the page last said.
    pub fn said(&self) -> String {
        self.imp().said.borrow().clone()
    }

    fn widgets(&self) -> &Widgets {
        self.imp().widgets.get().expect("built")
    }

    fn say(&self, text: &str) {
        *self.imp().said.borrow_mut() = text.to_string();
        self.widgets().toasts.add_toast(adw::Toast::new(text));
    }

    fn forget(&self) {
        let imp = self.imp();
        imp.source.borrow_mut().take();
        imp.selected.borrow_mut().clear();
        imp.anchor.borrow_mut().take();
        imp.groups.borrow_mut().clear();
    }

    /// The library was read again: after an apply the groups are written or stale, so they go.
    fn reload(&self) {
        if self.imp().event.borrow().is_none() {
            return;
        }
        self.forget();
        self.load(false);
    }

    fn load(&self, fit: bool) {
        let imp = self.imp();
        let (Some(library), Some(event)) = (imp.library.borrow().clone(), imp.event.borrow().clone()) else {
            return;
        };
        imp.loading.set(true);
        let page = self.downgrade();
        let asked = event.clone();
        library.timeline(&asked, move |read| {
            let Some(page) = page.upgrade() else {
                return;
            };
            if page.imp().event.borrow().as_deref() != Some(event.as_str()) {
                return;
            }
            page.imp().loading.set(false);
            match read {
                Ok(timeline) => {
                    tracing::info!(
                        event,
                        lanes = timeline.lanes.len(),
                        undated = timeline.undated.len(),
                        "neighbour timeline read"
                    );
                    if fit {
                        page.imp().zoom.set(fitting(&timeline));
                    }
                    *page.imp().timeline.borrow_mut() = Some(timeline);
                    page.show_lanes();
                    page.draw();
                    page.show_state();
                }
                Err(why) => {
                    tracing::error!(why, "the timeline could not be read");
                    page.say(&format!("The timeline could not be read: {why}"));
                }
            }
        });
    }

    fn build(&self) {
        let title = adw::WindowTitle::new("Position from a Neighbour", "");
        let preview = gtk::Button::builder()
            .label("Preview")
            .sensitive(false)
            .action_name("neighbour.preview")
            .build();
        preview.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .title_widget(&title)
            .build();
        header.pack_end(&preview);

        let map = gtk::Button::builder()
            .label("Show on Map")
            .valign(gtk::Align::Center)
            .visible(false)
            .action_name("neighbour.show-map")
            .build();
        map.add_css_class("flat");
        let source = adw::ActionRow::builder().title("Source").build();
        source.add_suffix(&map);
        let about = adw::PreferencesGroup::builder()
            .description(
                "Click a photo that measured its position, framed green, to make it the source. \
                 Select the photos taken at the same spot - click, Shift for a run, or drag a band \
                 across the lanes - and give them its position.",
            )
            .build();
        about.add(&source);

        let zoom = gtk::Label::new(None);
        zoom.add_css_class("dim-label");
        zoom.add_css_class("numeric");
        let zoom_out = gtk::Button::builder()
            .icon_name("zoom-out-symbolic")
            .tooltip_text("Zoom Out")
            .action_name("neighbour.zoom")
            .action_target(&"out".to_variant())
            .build();
        let zoom_in = gtk::Button::builder()
            .icon_name("zoom-in-symbolic")
            .tooltip_text("Zoom In")
            .action_name("neighbour.zoom")
            .action_target(&"in".to_variant())
            .build();
        let zooming = gtk::Box::builder().spacing(6).build();
        zooming.add_css_class("linked");
        zooming.append(&zoom_out);
        zooming.append(&zoom_in);
        let zoom_bar = gtk::Box::builder().spacing(12).build();
        zoom_bar.append(&zooming);
        zoom_bar.append(&zoom);

        let names = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        let fixed = gtk::Fixed::new();
        let band = gtk::Box::builder().visible(false).can_target(false).build();
        band.add_css_class("neighbour-band");
        fixed.put(&band, 0.0, 0.0);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&fixed)
            .build();
        let timeline = gtk::Box::builder().spacing(12).build();
        timeline.add_css_class("card");
        timeline.set_overflow(gtk::Overflow::Hidden);
        names.set_margin_start(12);
        timeline.append(&names);
        timeline.append(&scrolled);

        let undated_title = gtk::Label::builder().label("Without a Date").xalign(0.0).build();
        undated_title.add_css_class("heading");
        let undated = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .max_children_per_line(64)
            .column_spacing(GAP as u32)
            .row_spacing(GAP as u32)
            .build();

        let picked = gtk::Label::builder().xalign(0.0).valign(gtk::Align::Center).build();
        picked.set_margin_end(12);
        let reaches: Vec<String> = Reach::ALL
            .iter()
            .map(|reach| format!("{} ({})", reach.title(), reach.tells()))
            .collect();
        let reach = gtk::DropDown::from_strings(&reaches.iter().map(String::as_str).collect::<Vec<&str>>());
        reach.set_selected(reach_index(Reach::default()));
        reach.set_tooltip_text(Some("How far off the position may be"));
        reach.update_property(&[gtk::accessible::Property::Label("How Far Off It May Be")]);
        let none = gtk::Button::builder()
            .label("Select None")
            .action_name("neighbour.select-none")
            .build();
        let give = gtk::Button::builder()
            .label("Give Its Position")
            .sensitive(false)
            .action_name("neighbour.give")
            .build();
        give.add_css_class("suggested-action");
        let buttons = gtk::Box::builder().spacing(6).build();
        buttons.append(&none);
        buttons.append(&reach);
        let giving = adw::WrapBox::builder()
            .child_spacing(6)
            .line_spacing(6)
            .justify(adw::JustifyMode::None)
            .build();
        giving.append(&picked);
        giving.append(&buttons);
        giving.append(&give);

        let lanes = adw::PreferencesGroup::builder()
            .title("Cameras")
            .description(
                "Move a camera's photos along the axis when its clock was off. For the eye only: nothing is written.",
            )
            .build();
        let groups = adw::PreferencesGroup::builder()
            .title("Pending")
            .description("Nothing is written until the preview is applied")
            .visible(false)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        for part in [
            about.upcast_ref::<gtk::Widget>(),
            zoom_bar.upcast_ref(),
            timeline.upcast_ref(),
            undated_title.upcast_ref(),
            undated.upcast_ref(),
            giving.upcast_ref(),
            groups.upcast_ref(),
            lanes.upcast_ref(),
        ] {
            content.append(part);
        }
        let outer = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&outer));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view));
        self.set_child(Some(&toasts));

        let page = self.downgrade();
        reach.connect_selected_notify(move |reach| {
            if let Some(page) = page.upgrade() {
                let chosen = Reach::ALL[(reach.selected() as usize).min(Reach::ALL.len() - 1)];
                page.imp().reach.set(chosen);
            }
        });
        self.band_gesture(&fixed);

        let _ = self.imp().widgets.set(Widgets {
            title,
            toasts,
            preview,
            source,
            map,
            zoom,
            lanes,
            names,
            fixed,
            band,
            undated_title,
            undated,
            picked,
            reach,
            give,
            groups,
        });
        self.show_state();
    }

    fn install_actions(&self) {
        let group = gio::SimpleActionGroup::new();
        let text = |name: &str, act: fn(&NeighbourPage, &str)| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let page = self.downgrade();
            action.connect_activate(move |_, parameter| {
                if let (Some(page), Some(text)) = (page.upgrade(), parameter.and_then(|value| value.str())) {
                    act(&page, text);
                }
            });
            action
        };
        let plain = |name: &str, act: fn(&NeighbourPage)| {
            let action = gio::SimpleAction::new(name, None);
            let page = self.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(page) = page.upgrade() {
                    act(&page);
                }
            });
            action
        };
        let take_back = gio::SimpleAction::new("take-back", Some(glib::VariantTy::INT32));
        let page = self.downgrade();
        take_back.connect_activate(move |_, parameter| {
            if let (Some(page), Some(index)) = (page.upgrade(), parameter.and_then(|value| value.get::<i32>())) {
                page.take_back(index.max(0) as usize);
            }
        });
        for action in [
            text("source", |page, path| page.pick_source(path)),
            text("select", |page, path| page.toggle(path)),
            text("select-run", |page, path| page.select_run(path)),
            text("select-many", |page, paths| {
                let paths: Vec<String> = paths.lines().map(String::from).collect();
                page.select_many(&paths);
            }),
            text("reach", |page, key| match Reach::named(key) {
                Some(reach) => page.widgets().reach.set_selected(reach_index(reach)),
                None => tracing::warn!(reach = key, "no such reach"),
            }),
            text("shift", |page, written| page.shift(written)),
            text("zoom", |page, way| page.zoom(way)),
            plain("select-none", NeighbourPage::select_none),
            plain("give", NeighbourPage::give),
            plain("preview", NeighbourPage::preview),
            plain("show-map", NeighbourPage::show_map),
            take_back,
        ] {
            group.add_action(&action);
        }
        self.insert_action_group("neighbour", Some(&group));
    }

    fn photo(&self, rel_path: &str) -> Option<Moment> {
        self.imp().timeline.borrow().as_ref()?.find(rel_path).cloned()
    }

    /// A photo that measured its position becomes the source; any other is selected or not.
    fn clicked(&self, rel_path: &str, shift: bool) {
        match self.photo(rel_path) {
            Some(photo) if photo.position.is_measured() => self.pick_source(rel_path),
            Some(_) if shift => self.select_run(rel_path),
            Some(_) => self.toggle(rel_path),
            None => {}
        }
    }

    fn pick_source(&self, rel_path: &str) {
        match self.photo(rel_path) {
            Some(photo) if photo.position.is_measured() => {
                *self.imp().source.borrow_mut() = Some(rel_path.to_string());
                tracing::info!(source = rel_path, "neighbour source picked");
            }
            Some(photo) => self.say(&format!("{} did not measure its position", photo.name())),
            None => self.say("That photo is not in the event"),
        }
        self.show_state();
    }

    /// A photo that measured its position is never a target, however it is asked.
    fn selectable(&self, rel_path: &str) -> bool {
        self.photo(rel_path).is_some_and(|photo| !photo.position.is_measured())
    }

    fn toggle(&self, rel_path: &str) {
        if !self.selectable(rel_path) {
            self.say("A photo that measured its position is never given another");
            return;
        }
        {
            let mut selected = self.imp().selected.borrow_mut();
            if !selected.remove(rel_path) {
                selected.insert(rel_path.to_string());
            }
        }
        *self.imp().anchor.borrow_mut() = Some(rel_path.to_string());
        self.show_state();
    }

    /// Every photo between the last one clicked and this one: in its lane when both share one,
    /// else by the time the axis shows.
    fn select_run(&self, rel_path: &str) {
        let anchor = self.imp().anchor.borrow().clone();
        let Some(anchor) = anchor else {
            self.toggle(rel_path);
            return;
        };
        let run = self.run_between(&anchor, rel_path);
        self.select_many(&run);
        *self.imp().anchor.borrow_mut() = Some(rel_path.to_string());
    }

    fn run_between(&self, from: &str, to: &str) -> Vec<String> {
        let timeline = self.imp().timeline.borrow();
        let Some(timeline) = timeline.as_ref() else {
            return Vec::new();
        };
        let between = |list: &[Moment]| -> Option<Vec<String>> {
            let one = list.iter().position(|photo| photo.rel_path == from)?;
            let other = list.iter().position(|photo| photo.rel_path == to)?;
            let (first, last) = (one.min(other), one.max(other));
            Some(list[first..=last].iter().map(|photo| photo.rel_path.clone()).collect())
        };
        for list in timeline
            .lanes
            .iter()
            .map(|lane| lane.photos.as_slice())
            .chain([timeline.undated.as_slice()])
        {
            if let Some(run) = between(list) {
                return run;
            }
        }
        let shifts = self.imp().shifts.borrow();
        let at = |rel_path: &str| {
            let photo = timeline.find(rel_path)?;
            Some(photo.seconds? + shifts.get(&photo.camera).copied().unwrap_or_default())
        };
        let (Some(one), Some(other)) = (at(from), at(to)) else {
            return vec![to.to_string()];
        };
        let (first, last) = (one.min(other), one.max(other));
        timeline
            .lanes
            .iter()
            .flat_map(|lane| &lane.photos)
            .filter(|photo| at(&photo.rel_path).is_some_and(|at| (first..=last).contains(&at)))
            .map(|photo| photo.rel_path.clone())
            .collect()
    }

    /// Adds the photos to the selection, leaving out those that measured their position.
    fn select_many(&self, paths: &[String]) {
        let wanted: Vec<String> = paths.iter().filter(|path| self.selectable(path)).cloned().collect();
        self.imp().selected.borrow_mut().extend(wanted);
        self.show_state();
    }

    fn select_none(&self) {
        self.imp().selected.borrow_mut().clear();
        self.imp().anchor.borrow_mut().take();
        self.show_state();
    }

    /// The selection becomes a pending group with the source and the reach. Nothing is written.
    fn give(&self) {
        let imp = self.imp();
        let Some(source) = imp.source.borrow().clone() else {
            self.say("Pick a photo that measured its position first");
            return;
        };
        let targets: Vec<String> = imp.selected.borrow().iter().cloned().collect();
        if targets.is_empty() {
            self.say("Select the photos taken at the same spot first");
            return;
        }
        let reach = imp.reach.get();
        tracing::info!(
            source,
            photos = targets.len(),
            reach = reach.key(),
            "neighbour group given"
        );
        imp.groups.borrow_mut().push(Group { source, reach, targets });
        imp.selected.borrow_mut().clear();
        imp.anchor.borrow_mut().take();
        self.show_state();
    }

    fn take_back(&self, index: usize) {
        let taken = {
            let mut groups = self.imp().groups.borrow_mut();
            (index < groups.len()).then(|| groups.remove(index))
        };
        match taken {
            Some(group) => tracing::info!(source = group.source, "neighbour group taken back"),
            None => tracing::warn!(index, "no such group"),
        }
        self.show_state();
    }

    /// `1:+72` moves the second lane 72 minutes later, for the eye.
    fn shift(&self, written: &str) {
        let parsed = written.split_once(':').and_then(|(lane, minutes)| {
            Some((lane.trim().parse::<usize>().ok()?, minutes.trim().parse::<i64>().ok()?))
        });
        let Some((lane, minutes)) = parsed else {
            tracing::warn!(shift = written, "a shift is a lane and minutes");
            return;
        };
        let rows = self.imp().lane_rows.borrow().clone();
        match rows.get(lane).and_then(|row| row.downcast_ref::<adw::SpinRow>()) {
            Some(row) => row.set_value(minutes as f64),
            None => tracing::warn!(lane, "no such lane"),
        }
    }

    fn zoom(&self, way: &str) {
        let at = self.imp().zoom.get();
        let next = match way {
            "in" => at.saturating_sub(1),
            "out" => (at + 1).min(ZOOMS.len() - 1),
            _ => return,
        };
        self.imp().zoom.set(next);
        self.draw();
    }

    fn preview(&self) {
        let imp = self.imp();
        let Some(event) = imp.event.borrow().clone() else {
            return;
        };
        let groups = imp.groups.borrow().clone();
        if groups.is_empty() {
            self.say("Give a position to some photos first");
            return;
        }
        let Some(tools) = self.ancestor(Tools::static_type()).and_downcast::<Tools>() else {
            return;
        };
        tools.preview_edit(
            Edit::PositionFromNeighbour,
            Value::Neighbours(Neighbours { event, groups }),
        );
    }

    /// The source's position on a map, only when asked: the tiles come from the network.
    fn show_map(&self) {
        let Some((lat, lon)) = self
            .source()
            .and_then(|source| self.photo(&source))
            .and_then(|photo| photo.gps)
        else {
            return;
        };
        let (map, _) = crate::panel::map_at(lat, lon);
        map.set_height_request(360);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(6)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&map);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title("Where the Source Was")
            .content_width(520)
            .child(&view)
            .build();
        dialog.present(Some(self));
    }

    /// One row per camera, with its photos and the shift of its clock for the eye.
    fn show_lanes(&self) {
        let imp = self.imp();
        let widgets = self.widgets();
        for row in imp.lane_rows.borrow_mut().drain(..) {
            widgets.lanes.remove(&row);
        }
        let timeline = imp.timeline.borrow();
        let Some(timeline) = timeline.as_ref() else {
            return;
        };
        for lane in &timeline.lanes {
            let minutes = imp.shifts.borrow().get(&lane.camera).copied().unwrap_or_default() / 60;
            let adjustment = gtk::Adjustment::new(minutes as f64, -525_600.0, 525_600.0, 1.0, 60.0, 0.0);
            let row = adw::SpinRow::builder()
                .title(glib::markup_escape_text(&lane.camera))
                .subtitle(format!("{}, clock moved by minutes", photos(lane.photos.len())))
                .adjustment(&adjustment)
                .build();
            let camera = lane.camera.clone();
            let page = self.downgrade();
            row.connect_value_notify(move |row| {
                if let Some(page) = page.upgrade() {
                    page.imp()
                        .shifts
                        .borrow_mut()
                        .insert(camera.clone(), row.value() as i64 * 60);
                    page.draw();
                }
            });
            widgets.lanes.add(&row);
            imp.lane_rows.borrow_mut().push(row.upcast());
        }
        widgets.lanes.set_visible(!timeline.lanes.is_empty());
    }

    /// Lays the photos out along the axis: a lane per camera, crowded photos stacked.
    fn draw(&self) {
        let imp = self.imp();
        let widgets = self.widgets();
        let loader = imp.loader.borrow().clone();
        for (_, tile) in imp.tiles.borrow_mut().drain() {
            if let Some(loader) = &loader {
                loader.release(&tile.slot);
            }
            if tile.at.is_some() {
                widgets.fixed.remove(&tile.button);
            }
        }
        for widget in imp.drawn.borrow_mut().drain(..) {
            if widget.parent().as_ref() == Some(widgets.fixed.upcast_ref()) {
                widgets.fixed.remove(&widget);
            } else if let Some(parent) = widget.parent().and_downcast::<gtk::Box>() {
                parent.remove(&widget);
            }
        }
        widgets.undated.remove_all();

        let timeline = imp.timeline.borrow().clone();
        let Some(timeline) = timeline else {
            return;
        };
        let per_100 = ZOOMS[imp.zoom.get().min(ZOOMS.len() - 1)];
        widgets.zoom.set_label(&format!("100 px are {}", duration(per_100)));
        let shifts = imp.shifts.borrow().clone();
        let shifted = |photo: &Moment| {
            photo
                .seconds
                .map(|at| at + shifts.get(&photo.camera).copied().unwrap_or_default())
        };
        let (first, last) = timeline
            .lanes
            .iter()
            .flat_map(|lane| &lane.photos)
            .filter_map(shifted)
            .fold((i64::MAX, i64::MIN), |(first, last), at| (first.min(at), last.max(at)));
        let x_of = |at: i64| START + ((at - first) * 100 / per_100) as i32;
        let width = if first <= last {
            x_of(last) + TILE + START
        } else {
            START * 2
        };

        let spacer = gtk::Box::builder().height_request(AXIS).build();
        widgets.names.append(&spacer);
        imp.drawn.borrow_mut().push(spacer.upcast());

        let mut top = AXIS;
        for lane in &timeline.lanes {
            let mut rows: Vec<i32> = Vec::new();
            let mut placed = Vec::new();
            for photo in &lane.photos {
                let Some(at) = shifted(photo) else { continue };
                let x = x_of(at);
                let row = match rows.iter().position(|right| right + GAP <= x) {
                    Some(row) => row,
                    None if rows.len() < ROWS => {
                        rows.push(i32::MIN);
                        rows.len() - 1
                    }
                    None => rows
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, right)| **right)
                        .map(|(row, _)| row)
                        .unwrap_or_default(),
                };
                rows[row] = x + TILE;
                placed.push((photo, x, top + GAP + row as i32 * (TILE + GAP)));
            }
            let height = rows.len().max(1) as i32 * (TILE + GAP) + GAP;
            let back = gtk::Box::builder().width_request(width).height_request(height).build();
            back.add_css_class("neighbour-lane");
            widgets.fixed.put(&back, 0.0, top as f64);
            imp.drawn.borrow_mut().push(back.upcast());
            let name = gtk::Label::builder()
                .label(&lane.camera)
                .xalign(0.0)
                .height_request(height)
                .width_chars(12)
                .max_width_chars(18)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .tooltip_text(format!("{}, {}", lane.camera, photos(lane.photos.len())))
                .build();
            name.add_css_class("caption-heading");
            widgets.names.append(&name);
            imp.drawn.borrow_mut().push(name.upcast());
            for (photo, x, y) in placed {
                let tile = self.tile(photo, Some((x, y)));
                widgets.fixed.put(&tile.button, x as f64, y as f64);
                imp.tiles.borrow_mut().insert(photo.rel_path.clone(), tile);
            }
            top += height;
        }
        if first <= last {
            self.draw_axis(first, last, per_100, &x_of);
        }
        widgets.fixed.set_size_request(width, top + GAP);
        // The band stays above the photos.
        widgets.fixed.remove(&widgets.band);
        widgets.fixed.put(&widgets.band, 0.0, 0.0);

        for photo in &timeline.undated {
            let tile = self.tile(photo, None);
            widgets.undated.append(&tile.button);
            imp.tiles.borrow_mut().insert(photo.rel_path.clone(), tile);
        }
        widgets.undated.set_visible(!timeline.undated.is_empty());
        widgets.undated_title.set_visible(!timeline.undated.is_empty());
        self.restyle();
    }

    /// A tick with its time every so often, the date at the first of each day.
    fn draw_axis(&self, first: i64, last: i64, per_100: i64, x_of: &dyn Fn(i64) -> i32) {
        let widgets = self.widgets();
        let step = TICKS
            .into_iter()
            .find(|step| step * 100 / per_100 >= 110)
            .unwrap_or(TICKS[TICKS.len() - 1]);
        let mut at = first.div_euclid(step) * step;
        if at < first {
            at += step;
        }
        let mut day = String::new();
        while at <= last {
            let written = dates::from_seconds(at).map(dates::format).unwrap_or_default();
            let (date, time) = written.split_at(written.len().min(10));
            let label = match date != day {
                true => format!("{date} {}", time.trim().get(..5).unwrap_or_default()),
                false => time.trim().get(..5).unwrap_or_default().to_string(),
            };
            day = date.to_string();
            let tick = gtk::Label::new(Some(&label));
            tick.add_css_class("caption");
            tick.add_css_class("dim-label");
            tick.add_css_class("numeric");
            widgets.fixed.put(&tick, x_of(at) as f64, 6.0);
            self.imp().drawn.borrow_mut().push(tick.upcast());
            at += step;
        }
    }

    fn tile(&self, photo: &Moment, at: Option<(i32, i32)>) -> Tile {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .width_request(TILE)
            .height_request(TILE)
            .can_shrink(true)
            .build();
        let overlay = gtk::Overlay::builder().child(&picture).build();
        let mark = match &photo.position {
            Position::Measured => Some("mark-location-symbolic"),
            Position::Derived(_) => Some("find-location-symbolic"),
            Position::None => None,
        };
        if let Some(icon) = mark {
            let mark = gtk::Image::builder()
                .icon_name(icon)
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .margin_end(2)
                .margin_bottom(2)
                .build();
            mark.add_css_class("neighbour-mark");
            overlay.add_overlay(&mark);
        }
        let button = gtk::Button::builder()
            .child(&overlay)
            .width_request(TILE)
            .height_request(TILE)
            .tooltip_text(tooltip(photo))
            .build();
        button.add_css_class("neighbour-tile");
        button.add_css_class(match photo.position {
            Position::Measured => "measured",
            Position::Derived(_) => "derived",
            Position::None => "unplaced",
        });
        let shift = Rc::new(Cell::new(false));
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        press.connect_pressed(glib::clone!(
            #[strong]
            shift,
            move |gesture, _, _, _| shift.set(gesture.current_event_state().contains(gdk::ModifierType::SHIFT_MASK))
        ));
        button.add_controller(press);
        let rel_path = photo.rel_path.clone();
        let page = self.downgrade();
        button.connect_clicked(move |_| {
            if let Some(page) = page.upgrade() {
                page.clicked(&rel_path, shift.replace(false));
            }
        });

        let slot = Slot::default();
        let loader = self.imp().loader.borrow().clone();
        let library = self.imp().library.borrow().clone();
        if let (Some(loader), Some(library)) = (loader, library) {
            let request = Request {
                key: photo.content_id.clone().unwrap_or_else(|| photo.rel_path.clone()),
                content_id: photo.content_id.clone(),
                file: library.paths().library().join(&photo.rel_path),
                orientation: photo.orientation,
            };
            let picture = picture.downgrade();
            loader.load(&slot, request, move |texture| {
                if let Some(picture) = picture.upgrade() {
                    picture.set_paintable(texture.as_ref());
                }
            });
        }
        Tile { button, slot, at }
    }

    /// Frames and colours: the source, the selection, and each photo's group.
    fn restyle(&self) {
        let imp = self.imp();
        let source = imp.source.borrow().clone();
        let selected = imp.selected.borrow().clone();
        let groups = imp.groups.borrow().clone();
        let neighbours = Neighbours {
            event: String::new(),
            groups: groups.clone(),
        };
        let owners = neighbours.owners();
        let timeline = imp.timeline.borrow();
        for (rel_path, tile) in imp.tiles.borrow().iter() {
            let button = &tile.button;
            for index in 0..PALETTE {
                button.remove_css_class(&format!("neighbour-group-{index}"));
            }
            let is_source = source.as_deref() == Some(rel_path.as_str());
            let is_selected = selected.contains(rel_path);
            let owner = owners
                .get(rel_path.as_str())
                .copied()
                .or_else(|| groups.iter().rposition(|group| group.source == *rel_path));
            set_class(button, "source", is_source);
            set_class(button, "picked", is_selected);
            if let Some(index) = owner {
                button.add_css_class(&format!("neighbour-group-{}", index % PALETTE));
            }
            let Some(photo) = timeline.as_ref().and_then(|timeline| timeline.find(rel_path)) else {
                continue;
            };
            let mut label = photo.label();
            if is_source {
                label.push_str(", the source");
            }
            if is_selected {
                label.push_str(", selected");
            }
            if let Some(index) = owners.get(rel_path.as_str()) {
                let from = &groups[*index].source;
                label.push_str(&format!(", to get the position of {}", name_of(from)));
            }
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
        }
    }

    /// The source line, what is selected, the pending groups, and what can be pressed.
    fn show_state(&self) {
        let imp = self.imp();
        let widgets = self.widgets();
        let timeline = imp.timeline.borrow().clone();
        let measured = timeline
            .as_ref()
            .is_some_and(|timeline| timeline.photos().any(|photo| photo.position.is_measured()));
        let source = imp.source.borrow().clone();
        match (&source, &timeline) {
            (Some(source), _) => widgets
                .source
                .set_subtitle(&glib::markup_escape_text(&format!("{}, measured", name_of(source)))),
            (None, None) => widgets.source.set_subtitle("Reading the event"),
            (None, Some(_)) if !measured => widgets
                .source
                .set_subtitle("No photo of this event measured its position"),
            (None, Some(_)) => widgets.source.set_subtitle("Click a photo framed green"),
        }
        widgets.map.set_visible(source.is_some());

        let selected = imp.selected.borrow().len();
        widgets.picked.set_label(&match selected {
            0 => "Nothing selected".to_string(),
            1 => "1 photo selected".to_string(),
            count => format!("{count} photos selected"),
        });
        widgets.give.set_sensitive(source.is_some() && selected > 0);

        for row in imp.group_rows.borrow_mut().drain(..) {
            widgets.groups.remove(&row);
        }
        let groups = imp.groups.borrow().clone();
        for (index, group) in groups.iter().enumerate() {
            let names: Vec<&str> = group.targets.iter().map(|target| name_of(target)).collect();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&format!(
                    "From {}, {}",
                    name_of(&group.source),
                    group.reach.tells()
                )))
                .subtitle(glib::markup_escape_text(&format!(
                    "{}: {}",
                    photos(group.targets.len()),
                    names.join(", ")
                )))
                .subtitle_lines(2)
                .build();
            let dot = gtk::Box::builder()
                .width_request(12)
                .height_request(12)
                .valign(gtk::Align::Center)
                .build();
            dot.add_css_class("neighbour-dot");
            dot.add_css_class(&format!("neighbour-group-{}", index % PALETTE));
            row.add_prefix(&dot);
            let back = gtk::Button::builder()
                .label("Take Back")
                .valign(gtk::Align::Center)
                .action_name("neighbour.take-back")
                .action_target(&(index as i32).to_variant())
                .build();
            back.add_css_class("flat");
            // A button with a text is named by it; every group's would be "Take Back".
            back.reset_relation(gtk::AccessibleRelation::LabelledBy);
            back.update_property(&[gtk::accessible::Property::Label(&format!(
                "Take Back the Photos From {}",
                name_of(&group.source)
            ))]);
            row.add_suffix(&back);
            widgets.groups.add(&row);
            imp.group_rows.borrow_mut().push(row.upcast());
        }
        widgets.groups.set_visible(!groups.is_empty());
        widgets.preview.set_sensitive(!groups.is_empty());
        self.restyle();
    }

    /// A band dragged across the lanes selects the photos it touches; with Shift it adds to them.
    fn band_gesture(&self, fixed: &gtk::Fixed) {
        let drag = gtk::GestureDrag::new();
        let page = self.downgrade();
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(page) = page.upgrade() {
                page.imp().band_from.set(Some((x, y)));
            }
        });
        let page = self.downgrade();
        drag.connect_drag_update(move |_, dx, dy| {
            let Some(page) = page.upgrade() else { return };
            let Some((x, y)) = page.imp().band_from.get() else {
                return;
            };
            if dx.abs() + dy.abs() < 6.0 {
                return;
            }
            let band = &page.widgets().band;
            let (left, top) = (x.min(x + dx), y.min(y + dy));
            band.set_size_request(dx.abs() as i32, dy.abs() as i32);
            page.widgets().fixed.move_(band, left, top);
            band.set_visible(true);
        });
        let page = self.downgrade();
        drag.connect_drag_end(move |gesture, dx, dy| {
            let Some(page) = page.upgrade() else { return };
            page.widgets().band.set_visible(false);
            let Some((x, y)) = page.imp().band_from.take() else {
                return;
            };
            if dx.abs() + dy.abs() < 6.0 {
                return;
            }
            let (left, right) = (x.min(x + dx), x.max(x + dx));
            let (top, bottom) = (y.min(y + dy), y.max(y + dy));
            let touched: Vec<String> = page
                .imp()
                .tiles
                .borrow()
                .iter()
                .filter_map(|(rel_path, tile)| {
                    let (tx, ty) = tile.at?;
                    let (tx, ty) = (tx as f64, ty as f64);
                    let size = TILE as f64;
                    (tx < right && tx + size > left && ty < bottom && ty + size > top).then(|| rel_path.clone())
                })
                .collect();
            if !gesture.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) {
                page.imp().selected.borrow_mut().clear();
            }
            page.select_many(&touched);
        });
        fixed.add_controller(drag);
    }
}

/// The zoom at which the whole event fits, else the widest there is.
fn fitting(timeline: &Timeline) -> usize {
    let Some((first, last)) = timeline.span() else {
        return 4;
    };
    ZOOMS
        .iter()
        .position(|per_100| (last - first) * 100 / per_100 <= FITS)
        .unwrap_or(ZOOMS.len() - 1)
}

fn reach_index(reach: Reach) -> u32 {
    Reach::ALL.iter().position(|each| *each == reach).unwrap_or(1) as u32
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    match on {
        true => widget.add_css_class(class),
        false => widget.remove_css_class(class),
    }
}

fn name_of(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

fn photos(count: usize) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}

/// `5 min`, `3 h`, `1 day`.
fn duration(seconds: i64) -> String {
    match seconds {
        s if s % 86_400 == 0 && s / 86_400 == 1 => "1 day".to_string(),
        s if s % 86_400 == 0 => format!("{} days", s / 86_400),
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s => format!("{} min", s / 60),
    }
}

/// Its name, when it was taken and what its position is.
fn tooltip(photo: &Moment) -> String {
    let when = photo.taken_at.as_deref().unwrap_or("no date");
    let position = match (&photo.position, photo.gps) {
        (Position::None, _) | (_, None) => photo.position.tells(),
        (position, Some((lat, lon))) => format!("{lat:.5}, {lon:.5}, {}", position.tells()),
    };
    format!("{}\n{when}\n{position}", photo.name())
}
