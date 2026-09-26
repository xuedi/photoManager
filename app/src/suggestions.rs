//! The Suggestions view: what the tools found in the library on their own, grouped by tool, the
//! biggest first. Open hands a suggestion's scope and settings to its tool, whose page, preview
//! and apply do the rest; nothing is ever written from here. Dismiss remembers the key.
//!
//! The tools are asked off the main thread, after every scan and whenever the view is shown.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use photomanager_core::tools::{self, Suggestion};

use crate::library::Library;

type Listener = Box<dyn Fn(usize)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Suggestions {
        pub toasts: adw::ToastOverlay,
        pub content: gtk::Box,
        pub loading: adw::StatusPage,
        pub none: adw::StatusPage,
        pub dismissed_group: adw::PreferencesGroup,
        pub show_dismissed: adw::SwitchRow,
        pub groups: RefCell<Vec<adw::PreferencesGroup>>,
        pub library: RefCell<Option<Rc<Library>>>,
        /// The newest that arrived, `None` until the first did.
        pub found: RefCell<Option<Vec<Suggestion>>>,
        pub dismissed: RefCell<BTreeSet<String>>,
        /// Which asking is the newest, so one that arrives late is dropped.
        pub asking: Cell<u64>,
        pub busy: Cell<bool>,
        pub toast: RefCell<String>,
        pub listeners: RefCell<Vec<Listener>>,
    }

    impl std::fmt::Debug for Suggestions {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?} suggestions", self.found.borrow().as_ref().map(Vec::len))
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
            *self.imp().dismissed.borrow_mut() = library.dismissed();
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

    /// Told how many suggestions there are, not counting the dismissed, whenever that changes.
    pub fn connect_listed(&self, listed: impl Fn(usize) + 'static) {
        self.imp().listeners.borrow_mut().push(Box::new(listed));
    }

    /// Asks every tool again. One asked before that arrives later is dropped.
    pub fn ask(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let asking = imp.asking.get() + 1;
        imp.asking.set(asking);
        imp.busy.set(true);
        self.show();

        let page = self.downgrade();
        library.suggestions(move |found| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let imp = page.imp();
            if imp.asking.get() != asking {
                return;
            }
            imp.busy.set(false);
            match found {
                Ok(found) => *imp.found.borrow_mut() = Some(found),
                Err(why) => tracing::error!(why, "the tools could not be asked for suggestions"),
            }
            page.show();
            page.tell();
        });
    }

    pub fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    /// Every suggestion found, the dismissed too.
    pub fn found(&self) -> Vec<Suggestion> {
        self.imp().found.borrow().clone().unwrap_or_default()
    }

    /// The ones not dismissed.
    pub fn open(&self) -> Vec<Suggestion> {
        let dismissed = self.imp().dismissed.borrow();
        self.found()
            .into_iter()
            .filter(|suggestion| !dismissed.contains(&suggestion.key))
            .collect()
    }

    pub fn find(&self, key: &str) -> Option<Suggestion> {
        self.found().into_iter().find(|suggestion| suggestion.key == key)
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    pub fn set_show_dismissed(&self, show: bool) {
        self.imp().show_dismissed.set_active(show);
    }

    /// Dismisses a suggestion for good, or brings it back.
    pub fn dismiss(&self, key: &str, dismissed: bool) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        library.set_dismissed(key, dismissed);
        *imp.dismissed.borrow_mut() = library.dismissed();
        tracing::info!(suggestion = key, dismissed, "suggestion dismissed");
        if dismissed {
            let title = self
                .find(key)
                .map(|suggestion| suggestion.title)
                .unwrap_or_else(|| key.to_string());
            let toast = adw::Toast::builder()
                .title(format!("Dismissed: {title}"))
                .button_label("Undo")
                .action_name("win.restore-suggestion")
                .action_target(&key.to_variant())
                .build();
            self.say(toast);
        }
        self.show();
        self.tell();
    }

    fn say(&self, toast: adw::Toast) {
        *self.imp().toast.borrow_mut() = toast.title().map(|title| title.to_string()).unwrap_or_default();
        self.imp().toasts.add_toast(toast);
    }

    fn tell(&self) {
        let count = self.open().len();
        for listener in self.imp().listeners.borrow().iter() {
            listener(count);
        }
    }

    fn show(&self) {
        let imp = self.imp();
        for group in imp.groups.borrow_mut().drain(..) {
            imp.content.remove(&group);
        }
        let found = imp.found.borrow().clone();
        imp.loading.set_visible(found.is_none());
        let Some(found) = found else {
            imp.none.set_visible(false);
            imp.dismissed_group.set_visible(false);
            return;
        };
        let dismissed = imp.dismissed.borrow().clone();
        let showing_dismissed = imp.show_dismissed.is_active();
        let shown: Vec<&Suggestion> = found
            .iter()
            .filter(|suggestion| showing_dismissed || !dismissed.contains(&suggestion.key))
            .collect();

        let mut by_tool: Vec<(&'static str, Vec<&Suggestion>)> = Vec::new();
        for suggestion in &shown {
            match by_tool.iter_mut().find(|(tool, _)| *tool == suggestion.tool) {
                Some((_, listed)) => listed.push(suggestion),
                None => by_tool.push((suggestion.tool, vec![suggestion])),
            }
        }
        let size = |listed: &Vec<&Suggestion>| listed.iter().map(|suggestion| suggestion.photos).sum::<usize>();
        by_tool.sort_by_key(|(_, listed)| std::cmp::Reverse(size(listed)));

        let mut after: gtk::Widget = imp.none.clone().upcast();
        for (key, listed) in &by_tool {
            let tool = tools::find(key);
            let group = adw::PreferencesGroup::builder()
                .title(tool.map(|tool| tool.title()).unwrap_or(key))
                .build();
            if let Some(tool) = tool {
                group.set_description(Some(tool.fixes()));
            }
            for suggestion in listed {
                group.add(&row(suggestion, dismissed.contains(&suggestion.key)));
            }
            imp.content.insert_child_after(&group, Some(&after));
            after = group.clone().upcast();
            imp.groups.borrow_mut().push(group);
        }

        imp.none.set_visible(shown.is_empty());
        let hidden = found
            .iter()
            .filter(|suggestion| dismissed.contains(&suggestion.key))
            .count();
        imp.dismissed_group.set_visible(hidden > 0 || showing_dismissed);
        imp.show_dismissed.set_subtitle(&match hidden {
            1 => "1 dismissed".to_string(),
            count => format!("{count} dismissed"),
        });
    }

    fn build(&self) {
        let imp = self.imp();

        imp.loading.set_title("Looking for Suggestions");
        imp.loading.set_child(Some(&adw::Spinner::new()));
        imp.loading.add_css_class("compact");

        imp.none.set_icon_name(Some("starred-symbolic"));
        imp.none.set_title("No Suggestions");
        imp.none.set_description(Some(
            "The tools found nothing they can fix on their own. They look again after every scan.",
        ));
        imp.none.add_css_class("compact");
        imp.none.set_visible(false);

        imp.show_dismissed.set_title("Show Dismissed");
        imp.show_dismissed.connect_active_notify(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.show()
        ));
        imp.dismissed_group.add(&imp.show_dismissed);
        imp.dismissed_group.set_visible(false);

        imp.content.set_orientation(gtk::Orientation::Vertical);
        imp.content.set_spacing(24);
        imp.content.set_margin_top(24);
        imp.content.set_margin_bottom(24);
        imp.content.set_margin_start(12);
        imp.content.set_margin_end(12);
        imp.content.append(&imp.loading);
        imp.content.append(&imp.none);
        imp.content.append(&imp.dismissed_group);

        let clamp = adw::Clamp::builder().maximum_size(720).child(&imp.content).build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        imp.toasts.set_child(Some(&scrolled));
        self.set_child(Some(&imp.toasts));
    }
}

/// One suggestion: what it is about, Open, and Dismiss or Restore. What it is made of opens below
/// it where there is anything.
fn row(suggestion: &Suggestion, dismissed: bool) -> gtk::Widget {
    let subtitle = match (dismissed, photos(suggestion.photos)) {
        (true, photos) => format!("Dismissed - {} - {photos}", suggestion.detail),
        (false, photos) => format!("{} - {photos}", suggestion.detail),
    };
    let title = glib::markup_escape_text(&suggestion.title);
    let subtitle = glib::markup_escape_text(&subtitle);
    let open = gtk::Button::builder()
        .label("Open")
        .valign(gtk::Align::Center)
        .action_name("win.open-suggestion")
        .action_target(&suggestion.key.to_variant())
        .tooltip_text(match suggestion.sure {
            true => "Open it in its tool, to preview and apply",
            false => "Open its tool, to decide there",
        })
        .build();
    // A button is named by its label unless told otherwise; each row's Open needs its own name.
    open.reset_relation(gtk::AccessibleRelation::LabelledBy);
    open.update_property(&[gtk::accessible::Property::Label(&format!("Open {}", suggestion.title))]);
    let other = match dismissed {
        true => gtk::Button::builder()
            .label("Restore")
            .action_name("win.restore-suggestion")
            .build(),
        false => gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Dismiss")
            .action_name("win.dismiss-suggestion")
            .build(),
    };
    other.set_valign(gtk::Align::Center);
    other.set_action_target_value(Some(&suggestion.key.to_variant()));
    other.add_css_class("flat");
    other.update_property(&[gtk::accessible::Property::Label(&match dismissed {
        true => format!("Restore {}", suggestion.title),
        false => format!("Dismiss {}", suggestion.title),
    })]);

    if suggestion.rows.is_empty() {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .subtitle_lines(3)
            .build();
        row.add_suffix(&open);
        row.add_suffix(&other);
        return row.upcast();
    }
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle)
        .subtitle_lines(3)
        .build();
    // An expander row puts each suffix before the ones it has.
    row.add_suffix(&other);
    row.add_suffix(&open);
    for (name, said) in &suggestion.rows {
        row.add_row(
            &adw::ActionRow::builder()
                .title(glib::markup_escape_text(name))
                .subtitle(glib::markup_escape_text(said))
                .build(),
        );
    }
    row.upcast()
}

fn photos(count: usize) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}
