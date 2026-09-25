//! The folder layout, chosen from the presets or put together level by level: each level a row
//! that is dragged into place, or moved with its menu from the keyboard. The event folder is
//! always last. An example of the library's own shows what the layout makes of it, and saving
//! says how many events are then off it - nothing moves until Folder Migration moves them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use photomanager_core::layout::{Component, Layout, Level, PRESETS, Placement};

use crate::library::Library;

const DESCRIBED: &str = "The folders above each event, from the top of the library down. Changing the layout \
                         moves nothing: Folder Migration then offers to move the events into it.";

/// Told a sentence about what happened.
type Told = Box<dyn Fn(&str)>;

pub struct LayoutEditor {
    inner: Rc<Inner>,
}

struct Inner {
    library: Rc<Library>,
    levels: RefCell<Vec<Level>>,
    /// The layout kept in the settings.
    saved: RefCell<Layout>,
    sample: Option<Placement>,
    group: adw::PreferencesGroup,
    presets: adw::ComboRow,
    rows: RefCell<Vec<gtk::Widget>>,
    example: adw::ActionRow,
    /// What the example row says, as text.
    shown: RefCell<String>,
    save: adw::ButtonRow,
    /// A preset is being put in place, so the rows changing is not the user making it their own.
    choosing: Cell<bool>,
    saved_with: RefCell<Option<Told>>,
}

impl LayoutEditor {
    pub fn new(library: Rc<Library>) -> LayoutEditor {
        let saved = library.layout();
        let names: Vec<&str> = PRESETS.iter().map(|(name, _)| *name).chain(["Custom"]).collect();
        let presets = adw::ComboRow::builder()
            .title("Layout")
            .model(&gtk::StringList::new(&names))
            .build();
        let group = adw::PreferencesGroup::builder()
            .title("Folder Layout")
            .description(DESCRIBED)
            .build();
        group.add(&presets);
        let example = adw::ActionRow::builder()
            .title("Example")
            .subtitle_lines(3)
            .subtitle_selectable(true)
            .build();
        example.add_css_class("property");
        let save = adw::ButtonRow::builder().title("Save Layout").build();
        save.add_css_class("suggested-action");

        let inner = Rc::new(Inner {
            sample: library.sample_event(),
            library,
            levels: RefCell::new(saved.levels.clone()),
            saved: RefCell::new(saved),
            group,
            presets,
            rows: RefCell::new(Vec::new()),
            example,
            shown: RefCell::new(String::new()),
            save,
            choosing: Cell::new(false),
            saved_with: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        inner.presets.connect_selected_notify(move |row| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.choosing.get() {
                return;
            }
            if let Some((_, text)) = PRESETS.get(row.selected() as usize) {
                let preset = Layout::read(text).expect("a preset reads");
                inner.set_levels(preset.levels);
            }
        });
        let weak = Rc::downgrade(&inner);
        inner.save.connect_activated(move |row| {
            if let Some(inner) = weak.upgrade() {
                inner.ask_to_save(row);
            }
        });
        inner.refresh();
        LayoutEditor { inner }
    }

    /// The groups to put on a preferences page, in order.
    pub fn groups(&self) -> [adw::PreferencesGroup; 2] {
        let saving = adw::PreferencesGroup::new();
        saving.add(&self.inner.example);
        saving.add(&self.inner.save);
        [self.inner.group.clone(), saving]
    }

    /// Told with a sentence whenever a layout was saved.
    pub fn connect_saved(&self, saved: impl Fn(&str) + 'static) {
        *self.inner.saved_with.borrow_mut() = Some(Box::new(saved));
    }

    /// The layout as it is put together now, or why it cannot be used.
    pub fn layout(&self) -> Result<Layout, String> {
        self.inner.layout()
    }

    pub fn titles(&self) -> Vec<String> {
        self.inner
            .levels
            .borrow()
            .iter()
            .map(|level| level.component.title())
            .collect()
    }

    pub fn preset(&self) -> Option<String> {
        let names: Vec<&str> = PRESETS.iter().map(|(name, _)| *name).collect();
        names
            .get(self.inner.presets.selected() as usize)
            .map(|name| name.to_string())
    }

    pub fn choose_preset(&self, name: &str) {
        let at = PRESETS
            .iter()
            .position(|(preset, _)| *preset == name)
            .expect("a preset");
        self.inner.presets.set_selected(at as u32);
    }

    pub fn move_level(&self, from: usize, to: usize) {
        self.inner.move_level(from, to);
    }

    pub fn toggle_optional(&self, at: usize) {
        self.inner.toggle_optional(at);
    }

    pub fn remove(&self, at: usize) {
        self.inner.remove(at);
    }

    pub fn add(&self, component: Component) {
        self.inner.add(component);
    }

    pub fn can_save(&self) -> bool {
        self.inner.save.is_sensitive()
    }

    pub fn example(&self) -> String {
        self.inner.shown.borrow().clone()
    }

    pub fn problem(&self) -> Option<String> {
        self.layout().err()
    }

    /// Keeps the layout without asking, as the Save button does once it was confirmed.
    pub fn save_now(&self) -> Result<(), String> {
        self.inner.save_now()
    }
}

impl Inner {
    fn layout(&self) -> Result<Layout, String> {
        let layout = Layout {
            levels: self.levels.borrow().clone(),
        };
        layout.check()?;
        Ok(layout)
    }

    fn set_levels(self: &Rc<Self>, levels: Vec<Level>) {
        *self.levels.borrow_mut() = levels;
        self.refresh();
    }

    fn move_level(self: &Rc<Self>, from: usize, to: usize) {
        let mut levels = self.levels.borrow().clone();
        if from >= levels.len() || to >= levels.len() || from == to {
            return;
        }
        let level = levels.remove(from);
        levels.insert(to, level);
        self.set_levels(levels);
    }

    fn toggle_optional(self: &Rc<Self>, at: usize) {
        let mut levels = self.levels.borrow().clone();
        if let Some(level) = levels.get_mut(at) {
            level.optional = !level.optional;
        }
        self.set_levels(levels);
    }

    fn remove(self: &Rc<Self>, at: usize) {
        let mut levels = self.levels.borrow().clone();
        if at < levels.len() {
            levels.remove(at);
        }
        self.set_levels(levels);
    }

    fn add(self: &Rc<Self>, component: Component) {
        let mut levels = self.levels.borrow().clone();
        levels.push(Level {
            component,
            optional: false,
        });
        self.set_levels(levels);
    }

    /// Puts the rows, the preset, the example and the Save button in line with the levels.
    fn refresh(self: &Rc<Self>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        let levels = self.levels.borrow().clone();
        let mut rows: Vec<gtk::Widget> = Vec::new();
        for (at, level) in levels.iter().enumerate() {
            rows.push(self.level_row(at, level, levels.len()).upcast());
        }
        let event = adw::ActionRow::builder()
            .title("Event")
            .subtitle("YYYY-MM-DD Name, always the last folder")
            .build();
        event.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        rows.push(event.upcast());
        rows.push(self.add_row(&levels).upcast());
        for row in &rows {
            self.group.add(row);
        }
        *self.rows.borrow_mut() = rows;

        let layout = self.layout();
        self.choosing.set(true);
        let preset = layout
            .as_ref()
            .ok()
            .and_then(|layout| PRESETS.iter().position(|(_, text)| *text == layout.to_string()))
            .unwrap_or(PRESETS.len());
        self.presets.set_selected(preset as u32);
        self.choosing.set(false);

        match &layout {
            Ok(layout) => {
                self.group.set_description(Some(DESCRIBED));
                let shown = match &self.sample {
                    Some(sample) => layout.example(sample),
                    None => layout.title(),
                };
                self.example.set_subtitle(&glib::markup_escape_text(&shown));
                *self.shown.borrow_mut() = shown;
            }
            Err(why) => {
                self.group
                    .set_description(Some(&format!("This layout cannot be used: {why}.")));
                self.example.set_subtitle("");
                self.shown.borrow_mut().clear();
            }
        }
        let changed = layout.as_ref().is_ok_and(|layout| *layout != *self.saved.borrow());
        self.save.set_sensitive(changed);
    }

    fn level_row(self: &Rc<Self>, at: usize, level: &Level, count: usize) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(level.component.title())
            .subtitle(match level.optional {
                true => "Optional: an event without one sits in the level above",
                false => "",
            })
            .build();
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.set_tooltip_text(Some("Drag to Move"));
        row.add_prefix(&handle);

        let actions = gio::SimpleActionGroup::new();
        let later = |inner: &Rc<Inner>, change: fn(&Rc<Inner>, usize)| {
            let weak = Rc::downgrade(inner);
            move || {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(inner) = weak.upgrade() {
                        change(&inner, at);
                    }
                });
            }
        };
        let up = gio::SimpleAction::new("up", None);
        up.set_enabled(at > 0);
        let go = later(self, |inner, at| inner.move_level(at, at - 1));
        up.connect_activate(move |_, _| go());
        let down = gio::SimpleAction::new("down", None);
        down.set_enabled(at + 1 < count);
        let go = later(self, |inner, at| inner.move_level(at, at + 1));
        down.connect_activate(move |_, _| go());
        let optional = gio::SimpleAction::new_stateful("optional", None, &level.optional.to_variant());
        let go = later(self, |inner, at| inner.toggle_optional(at));
        optional.connect_activate(move |_, _| go());
        let remove = gio::SimpleAction::new("remove", None);
        let go = later(self, |inner, at| inner.remove(at));
        remove.connect_activate(move |_, _| go());
        for action in [&up, &down, &optional, &remove] {
            actions.add_action(action);
        }
        row.insert_action_group("level", Some(&actions));

        let menu = gio::Menu::new();
        let moving = gio::Menu::new();
        moving.append(Some("Move _Up"), Some("level.up"));
        moving.append(Some("Move _Down"), Some("level.down"));
        menu.append_section(None, &moving);
        let kind = gio::Menu::new();
        kind.append(Some("_Optional"), Some("level.optional"));
        menu.append_section(None, &kind);
        let removing = gio::Menu::new();
        removing.append(Some("_Remove"), Some("level.remove"));
        menu.append_section(None, &removing);
        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Level Menu")
            .valign(gtk::Align::Center)
            .menu_model(&menu)
            .build();
        button.add_css_class("flat");
        row.add_suffix(&button);

        let drag = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&(at as u32).to_value())));
        let dragged = row.downgrade();
        drag.connect_drag_begin(move |source, _| {
            if let Some(row) = dragged.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        });
        row.add_controller(drag);
        let drop = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(inner), Ok(from)) = (weak.upgrade(), value.get::<u32>()) else {
                return false;
            };
            let weak = Rc::downgrade(&inner);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.move_level(from as usize, at);
                }
            });
            true
        });
        row.add_controller(drop);
        row
    }

    /// A row with a menu of the levels not used yet; a tag level for each top-level tag.
    fn add_row(self: &Rc<Self>, levels: &[Level]) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title("Add Level").build();
        let used = |component: &Component| levels.iter().any(|level| level.component == *component);
        let menu = gio::Menu::new();
        let tags = gio::Menu::new();
        for kind in Component::kinds() {
            match kind {
                Component::Tag(_) => {
                    if levels.iter().any(|level| matches!(level.component, Component::Tag(_))) {
                        continue;
                    }
                    for root in self.library.tag_roots() {
                        if root.contains('/') || root.trim() != root {
                            continue;
                        }
                        let item = gio::MenuItem::new(Some(&root), None);
                        item.set_action_and_target_value(Some("add.level"), Some(&format!("tag:{root}").to_variant()));
                        tags.append_item(&item);
                    }
                }
                kind if !used(&kind) => {
                    let item = gio::MenuItem::new(Some(&kind.title()), None);
                    let key = Layout {
                        levels: vec![Level {
                            component: kind,
                            optional: false,
                        }],
                    }
                    .to_string();
                    item.set_action_and_target_value(Some("add.level"), Some(&key.to_variant()));
                    menu.append_item(&item);
                }
                _ => {}
            }
        }
        if tags.n_items() > 0 {
            menu.append_submenu(Some("Tag Under"), &tags);
        }
        let actions = gio::SimpleActionGroup::new();
        let add = gio::SimpleAction::new("level", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        add.connect_activate(move |_, key| {
            let Some(key) = key.and_then(|key| key.get::<String>()) else {
                return;
            };
            let Ok(Some(level)) = Layout::read(&key).map(|layout| layout.levels.into_iter().next()) else {
                return;
            };
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.add(level.component);
                }
            });
        });
        actions.add_action(&add);
        row.insert_action_group("add", Some(&actions));
        let button = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add Level")
            .valign(gtk::Align::Center)
            .menu_model(&menu)
            .sensitive(menu.n_items() > 0)
            .build();
        button.add_css_class("flat");
        row.add_suffix(&button);
        row.set_activatable_widget(Some(&button));
        row
    }

    fn ask_to_save(self: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        let Ok(layout) = self.layout() else { return };
        let body = match self.library.events_off(&layout) {
            Some((0, _)) => "Every event is in this layout already.".to_string(),
            Some((1, events)) => {
                format!("1 of {events} events is not in this layout. Nothing moves until you run Folder Migration.")
            }
            Some((off, events)) => {
                format!(
                    "{off} of {events} events are not in this layout. Nothing moves until you run Folder Migration."
                )
            }
            None => "Nothing moves until you run Folder Migration.".to_string(),
        };
        let dialog = adw::AlertDialog::new(Some("Use This Layout?"), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("use", "Use Layout")]);
        dialog.set_response_appearance("use", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("use"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "use" {
                return;
            }
            let Some(inner) = weak.upgrade() else { return };
            let told = match inner.save_now() {
                Ok(()) => "The layout is kept".to_string(),
                Err(why) => why,
            };
            if let Some(saved_with) = inner.saved_with.borrow().as_ref() {
                saved_with(&told);
            }
        });
        dialog.present(Some(parent));
    }

    fn save_now(self: &Rc<Self>) -> Result<(), String> {
        let layout = self.layout()?;
        self.library.set_layout(&layout)?;
        tracing::info!(layout = %layout, "folder layout kept");
        *self.saved.borrow_mut() = layout;
        self.refresh();
        Ok(())
    }
}
