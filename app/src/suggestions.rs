//! The Suggestions view: every fix the app is sure about, grouped by finder in the order they are
//! applied, each with a check. Apply Selected writes the ticked ones, a pass for each finder, and
//! the list is found again: what was applied is gone because the photos now say it.
//!
//! The checks live only here, in memory. The fixes are found off the main thread, after every
//! scan and whenever the view is shown.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use photomanager_core::fixes::{FINDERS, Fix, Pass};

use crate::library::{Event, Library};

type Listener = Box<dyn Fn(usize)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Suggestions {
        pub toasts: adw::ToastOverlay,
        pub content: gtk::Box,
        pub loading: adw::StatusPage,
        pub none: adw::StatusPage,
        pub groups: RefCell<Vec<adw::PreferencesGroup>>,
        pub checks: RefCell<BTreeMap<String, gtk::CheckButton>>,
        /// The Select All of each group, by finder, which turns to Unselect All once all are ticked.
        pub everies: RefCell<Vec<(&'static str, &'static str, gtk::Button)>>,
        pub bar: gtk::ActionBar,
        pub chosen: gtk::Label,
        pub apply: gtk::Button,
        pub cancel: gtk::Button,
        pub progress: gtk::ProgressBar,
        pub library: RefCell<Option<Rc<Library>>>,
        /// The newest that arrived, `None` until the first did.
        pub found: RefCell<Option<Vec<Fix>>>,
        pub ticked: RefCell<BTreeSet<String>>,
        /// Which finding is the newest, so one that arrives late is dropped.
        pub asking: Cell<u64>,
        pub busy: Cell<bool>,
        pub applying: Cell<bool>,
        /// Checks set from code, which are no clicks.
        pub setting: Cell<bool>,
        pub toast: RefCell<String>,
        pub applied: RefCell<Option<Vec<Pass>>>,
        pub listeners: RefCell<Vec<Listener>>,
    }

    impl std::fmt::Debug for Suggestions {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?} fixes", self.found.borrow().as_ref().map(Vec::len))
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Suggestions {
        const NAME: &'static str = "PmSuggestions";
        type Type = super::Suggestions;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for Suggestions {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for Suggestions {}
    impl BinImpl for Suggestions {}
}

glib::wrapper! {
    pub struct Suggestions(ObjectSubclass<imp::Suggestions>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Suggestions {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Suggestions {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        if let Some(library) = &library {
            let page = self.downgrade();
            library.connect_changed(move || {
                if let Some(page) = page.upgrade() {
                    page.ask();
                }
            });
        }
        *self.imp().library.borrow_mut() = library;
        self.ask();
    }

    /// Told how many fixes there are, whenever that changes.
    pub fn connect_listed(&self, listed: impl Fn(usize) + 'static) {
        self.imp().listeners.borrow_mut().push(Box::new(listed));
    }

    /// Finds the fixes again. One found before that arrives later is dropped.
    pub fn ask(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let asking = imp.asking.get() + 1;
        imp.asking.set(asking);
        imp.busy.set(true);

        let page = self.downgrade();
        library.fixes(move |found| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let imp = page.imp();
            if imp.asking.get() != asking {
                return;
            }
            imp.busy.set(false);
            match found {
                Ok(found) => {
                    let keys: BTreeSet<String> = found.iter().map(|fix| fix.key.clone()).collect();
                    imp.ticked.borrow_mut().retain(|key| keys.contains(key));
                    *imp.found.borrow_mut() = Some(found);
                }
                Err(why) => tracing::error!(why, "the suggestions could not be found"),
            }
            page.show();
            page.tell();
        });
    }

    pub fn is_busy(&self) -> bool {
        self.imp().busy.get() || self.imp().applying.get()
    }

    pub fn found(&self) -> Vec<Fix> {
        self.imp().found.borrow().clone().unwrap_or_default()
    }

    pub fn ticked(&self) -> Vec<String> {
        self.imp().ticked.borrow().iter().cloned().collect()
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    /// What the last Apply Selected came to, a pass for each finder.
    pub fn applied(&self) -> Option<Vec<Pass>> {
        self.imp().applied.borrow().clone()
    }

    /// Ticks one fix, or takes the tick away.
    pub fn tick(&self, key: &str, ticked: bool) {
        let check = self.imp().checks.borrow().get(key).cloned();
        match check {
            Some(check) => check.set_active(ticked),
            None => tracing::warn!(fix = key, "no such suggestion"),
        }
    }

    /// Ticks every fix of a finder, or of all of them with `all`, or takes the ticks away.
    pub fn tick_every(&self, finder: &str, ticked: bool) {
        let keys: Vec<String> = self
            .found()
            .iter()
            .filter(|fix| finder == "all" || fix.finder == finder)
            .map(|fix| fix.key.clone())
            .collect();
        for key in keys {
            self.tick(&key, ticked);
        }
    }

    /// Asks before the first write of all, then writes the ticked fixes.
    pub fn apply(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        if library.is_busy() || self.is_busy() {
            return;
        }
        let ticked = imp.ticked.borrow().clone();
        let chosen: Vec<Fix> = self
            .found()
            .into_iter()
            .filter(|fix| ticked.contains(&fix.key))
            .collect();
        if chosen.is_empty() {
            return;
        }
        let page = self.downgrade();
        crate::confirm::before_first_write(self, &library, move || {
            let Some(page) = page.upgrade() else {
                return;
            };
            let Some(library) = page.imp().library.borrow().clone() else {
                return;
            };
            if library.is_busy() {
                page.say("Something else is running, so nothing was written.");
                return;
            }
            tracing::info!(fixes = chosen.len(), "suggestions applied");
            page.running(true);
            let told = page.downgrade();
            library.apply_fixes(chosen.clone(), move |event| {
                if let Some(page) = told.upgrade() {
                    page.report(event);
                }
            });
        });
    }

    pub fn cancel(&self) {
        if let Some(library) = self.imp().library.borrow().as_ref() {
            library.cancel();
            self.imp().progress.set_text(Some("Stopping after this pass"));
        }
    }

    fn report(&self, event: Event) {
        let imp = self.imp();
        match event {
            Event::Done(done, total) => {
                imp.progress.set_fraction(done as f64 / total.max(1) as f64);
                imp.progress.set_text(Some(&format!("{done} of {total}")));
            }
            Event::Note(note) => {
                imp.progress.pulse();
                imp.progress.set_text(Some(&note));
            }
            Event::Fixed(passes) => {
                self.running(false);
                imp.ticked.borrow_mut().clear();
                self.say(&told(&passes));
                *imp.applied.borrow_mut() = Some(passes);
                self.show();
                if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
                    window.scan(photomanager_core::scan::Mode::Reconcile);
                }
            }
            Event::Failed(why) => {
                self.running(false);
                self.say(&format!("Did not work: {why}"));
                tracing::error!(why, "the suggestions were not applied");
            }
            _ => {}
        }
    }

    fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    fn running(&self, busy: bool) {
        let imp = self.imp();
        imp.applying.set(busy);
        imp.apply.set_visible(!busy);
        imp.cancel.set_visible(busy);
        imp.progress.set_visible(busy);
        imp.chosen.set_visible(!busy);
        imp.content.set_sensitive(!busy);
        if busy {
            imp.progress.set_fraction(0.0);
            imp.progress.set_text(Some("Writing"));
        }
    }

    fn tell(&self) {
        let count = self.found().len();
        for listener in self.imp().listeners.borrow().iter() {
            listener(count);
        }
    }

    /// The action bar says what is ticked; only the ticks changed, so nothing else is drawn again.
    fn show_ticked(&self) {
        let imp = self.imp();
        let ticked = imp.ticked.borrow();
        let found = self.found();
        let chosen = found.iter().filter(|fix| ticked.contains(&fix.key)).count();
        imp.chosen.set_label(&match chosen {
            0 => "Tick the fixes to apply".to_string(),
            1 => "1 fix selected".to_string(),
            count => format!("{count} fixes selected"),
        });
        imp.apply.set_sensitive(chosen > 0);

        for (finder, title, every) in imp.everies.borrow().iter() {
            let all = found
                .iter()
                .filter(|fix| fix.finder == *finder)
                .all(|fix| ticked.contains(&fix.key));
            let (label, action) = match all {
                true => ("Unselect All", "win.fixes-select-none"),
                false => ("Select All", "win.fixes-select-all"),
            };
            every.set_label(label);
            every.set_action_name(Some(action));
            every.update_property(&[gtk::accessible::Property::Label(&format!("{label} of {title}"))]);
        }
    }

    fn show(&self) {
        let imp = self.imp();
        for group in imp.groups.borrow_mut().drain(..) {
            imp.content.remove(&group);
        }
        imp.checks.borrow_mut().clear();
        imp.everies.borrow_mut().clear();
        let found = imp.found.borrow().clone();
        imp.loading.set_visible(found.is_none());
        let found = found.unwrap_or_default();
        imp.none.set_visible(imp.found.borrow().is_some() && found.is_empty());
        imp.bar.set_revealed(!found.is_empty());

        let mut after: gtk::Widget = imp.none.clone().upcast();
        for finder in &FINDERS {
            let own: Vec<&Fix> = found.iter().filter(|fix| fix.finder == finder.key).collect();
            if own.is_empty() {
                continue;
            }
            let group = adw::PreferencesGroup::builder()
                .title(finder.title)
                .description(finder.fixes)
                .build();
            let every = gtk::Button::builder()
                .valign(gtk::Align::Center)
                .action_target(&finder.key.to_variant())
                .build();
            every.add_css_class("flat");
            group.set_header_suffix(Some(&every));
            imp.everies.borrow_mut().push((finder.key, finder.title, every));
            for fix in own {
                group.add(&self.row(fix));
            }
            imp.content.insert_child_after(&group, Some(&after));
            after = group.clone().upcast();
            imp.groups.borrow_mut().push(group);
        }
        self.show_ticked();
    }

    /// One fix: its check, what it is about and how many photos, and its lines below it where it
    /// has any.
    fn row(&self, fix: &Fix) -> gtk::Widget {
        let imp = self.imp();
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .active(imp.ticked.borrow().contains(&fix.key))
            .build();
        check.update_property(&[gtk::accessible::Property::Label(&fix.title)]);
        let key = fix.key.clone();
        check.connect_toggled(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |check| {
                match check.is_active() {
                    true => page.imp().ticked.borrow_mut().insert(key.clone()),
                    false => page.imp().ticked.borrow_mut().remove(&key),
                };
                page.show_ticked();
            }
        ));
        imp.checks.borrow_mut().insert(fix.key.clone(), check.clone());
        let title = glib::markup_escape_text(&fix.title);
        let subtitle = glib::markup_escape_text(&format!("{} - {}", fix.detail, photos_of(fix.photos)));

        if fix.lines.is_empty() {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .subtitle_lines(2)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            return row.upcast();
        }
        let row = adw::ExpanderRow::builder()
            .title(title)
            .subtitle(subtitle)
            .subtitle_lines(2)
            .build();
        row.add_prefix(&check);
        for (name, said) in &fix.lines {
            row.add_row(
                &adw::ActionRow::builder()
                    .title(glib::markup_escape_text(name))
                    .subtitle(glib::markup_escape_text(said))
                    .subtitle_selectable(true)
                    .build(),
            );
        }
        row.upcast()
    }

    fn build(&self) {
        let imp = self.imp();

        imp.loading.set_title("Looking for Suggestions");
        imp.loading.set_child(Some(&adw::Spinner::new()));
        imp.loading.add_css_class("compact");

        imp.none.set_icon_name(Some("object-select-symbolic"));
        imp.none.set_title("No Suggestions");
        imp.none.set_description(Some(
            "Nothing the app is sure it can fix. What needs a decision is done with the tools.",
        ));
        imp.none.add_css_class("compact");
        imp.none.set_visible(false);

        imp.content.set_orientation(gtk::Orientation::Vertical);
        imp.content.set_spacing(24);
        imp.content.set_margin_top(24);
        imp.content.set_margin_bottom(24);
        imp.content.set_margin_start(12);
        imp.content.set_margin_end(12);
        imp.content.append(&imp.loading);
        imp.content.append(&imp.none);

        imp.chosen.add_css_class("dim-label");
        imp.progress.set_show_text(true);
        imp.progress.set_hexpand(true);
        imp.progress.set_valign(gtk::Align::Center);
        imp.progress.set_visible(false);
        imp.apply.set_label("Apply Selected");
        imp.apply.add_css_class("suggested-action");
        imp.apply.set_action_name(Some("win.apply-fixes"));
        imp.apply.set_sensitive(false);
        imp.cancel.set_label("Cancel");
        imp.cancel.set_action_name(Some("win.cancel-fixes"));
        imp.cancel.set_visible(false);
        imp.bar.pack_start(&imp.chosen);
        imp.bar.pack_start(&imp.progress);
        imp.bar.pack_end(&imp.apply);
        imp.bar.pack_end(&imp.cancel);
        imp.bar.set_revealed(false);

        let clamp = adw::Clamp::builder().maximum_size(720).child(&imp.content).build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        let view = adw::ToolbarView::new();
        view.set_content(Some(&scrolled));
        view.add_bottom_bar(&imp.bar);
        imp.toasts.set_child(Some(&view));
        self.set_child(Some(&imp.toasts));
    }
}

/// `Written 12 photos in 2 passes. 1 refused.`
fn told(passes: &[Pass]) -> String {
    let written: usize = passes.iter().map(|pass| pass.summary.written).sum();
    let refused: usize = passes.iter().map(|pass| pass.summary.refused).sum();
    let failed: usize = passes.iter().map(|pass| pass.summary.failed).sum();
    let cancelled = passes.iter().any(|pass| pass.summary.cancelled);
    let mut said = match passes.len() {
        0 => "Nothing was left to write".to_string(),
        1 => format!("Written {} in 1 pass", photos_of(written)),
        count => format!("Written {} in {count} passes", photos_of(written)),
    };
    if refused > 0 {
        said.push_str(&format!(", {refused} refused"));
    }
    if failed > 0 {
        said.push_str(&format!(", {failed} failed"));
    }
    if cancelled {
        said.push_str(", then stopped");
    }
    said
}

fn photos_of(count: usize) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}
