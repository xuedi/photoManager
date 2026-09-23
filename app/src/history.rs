//! Every pass written to the photos, newest first, and taking any of them back. A pass opens to
//! its photos and what each got. The journal is the only source: nothing here is kept apart.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use photomanager_core::history::{self, Pass};
use photomanager_core::journal::{Kind, Recorded};
use photomanager_core::write::Summary;

use crate::library::{Event, Library};

/// How many passes are listed at a time.
const PAGE: i64 = 50;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct History {
        pub toasts: adw::ToastOverlay,
        pub progress: gtk::ProgressBar,
        pub list: adw::PreferencesGroup,
        pub more: gtk::Button,
        pub empty: adw::StatusPage,
        pub rows: RefCell<Vec<adw::ActionRow>>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub passes: RefCell<Vec<Pass>>,
        /// How many passes are asked for: a page more every time Show More is pressed.
        pub wanted: Cell<i64>,
        pub detail: RefCell<Option<(i64, Vec<String>)>>,
        pub taken: RefCell<Option<Summary>>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for History {
        const NAME: &'static str = "PmHistory";
        type Type = super::History;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for History {
        fn constructed(&self) {
            self.parent_constructed();
            self.wanted.set(PAGE);
            self.obj().build();
        }
    }

    impl WidgetImpl for History {}
    impl BinImpl for History {}
}

glib::wrapper! {
    pub struct History(ObjectSubclass<imp::History>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for History {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl History {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        if let Some(library) = &library {
            let history = self.downgrade();
            library.connect_changed(move || {
                if let Some(history) = history.upgrade() {
                    history.refresh();
                }
            });
        }
        *self.imp().library.borrow_mut() = library;
        self.refresh();
    }

    /// The passes on screen, newest first.
    pub fn passes(&self) -> Vec<Pass> {
        self.imp().passes.borrow().clone()
    }

    /// The pass last opened, and a line for each of its photos.
    pub fn detail(&self) -> Option<(i64, Vec<String>)> {
        self.imp().detail.borrow().clone()
    }

    /// What the last pass taken back from here came to.
    pub fn taken(&self) -> Option<Summary> {
        self.imp().taken.borrow().clone()
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    /// Reads the journal again. While a pass has it out, what is on screen stays.
    pub fn refresh(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let Some(passes) = library.history(0, imp.wanted.get()) else {
            return;
        };
        for row in imp.rows.borrow_mut().drain(..) {
            imp.list.remove(&row);
        }
        for pass in &passes {
            let row = row(pass);
            imp.list.add(&row);
            imp.rows.borrow_mut().push(row);
        }
        imp.more.set_visible(passes.len() as i64 == imp.wanted.get());
        imp.list.set_visible(!passes.is_empty());
        imp.empty.set_visible(passes.is_empty());
        *imp.passes.borrow_mut() = passes;
    }

    pub fn show_more(&self) {
        let imp = self.imp();
        imp.wanted.set(imp.wanted.get() + PAGE);
        self.refresh();
    }

    /// The page of one pass: when, how many, and every photo with what it got.
    pub fn details(&self, batch: i64) -> Option<adw::NavigationPage> {
        let library = self.imp().library.borrow().clone()?;
        let (pass, photos) = library.pass(batch)?;
        let lines: Vec<String> = photos.iter().map(history::told).collect();
        *self.imp().detail.borrow_mut() = Some((batch, lines));
        Some(pass_page(&pass, &photos))
    }

    /// Asks, naming the photos changed since, then takes the pass back.
    pub fn take_back(&self, batch: i64) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        if library.is_busy() {
            self.say("Something else is running. Try again when it is done.");
            return;
        }
        let Some((pass, _)) = library.pass(batch) else {
            return;
        };
        if !pass.can_take_back() {
            self.say("That change cannot be taken back.");
            return;
        }
        let history = self.downgrade();
        let title = pass.title.clone();
        crate::confirm::before_take_back(self, &pass, move || {
            let Some(history) = history.upgrade() else {
                return;
            };
            let Some(library) = history.imp().library.borrow().clone() else {
                return;
            };
            if library.is_busy() {
                history.say("Something else is running, so nothing was taken back.");
                return;
            }
            tracing::info!(batch, title = title.as_str(), "taking a pass back");
            if let Some(window) = history.root().and_downcast::<crate::window::Window>() {
                window.tools().back_to_history();
            }
            history.running(true);
            let reported = history.clone();
            library.take_back(batch, move |event| reported.report(event));
        });
    }

    fn report(&self, event: Event) {
        match event {
            Event::Done(done, total) => {
                let progress = &self.imp().progress;
                progress.set_fraction(done as f64 / total.max(1) as f64);
                progress.set_text(Some(&format!("{done} of {total}")));
            }
            Event::Applied(_, summary) => {
                self.running(false);
                self.say(&told(&summary));
                *self.imp().taken.borrow_mut() = Some(summary);
                self.refresh();
                if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
                    window.scan(photomanager_core::scan::Mode::Reconcile);
                }
            }
            Event::Failed(why) => {
                self.running(false);
                self.say(&format!("Did not work: {why}"));
                tracing::error!(why, "the pass was not taken back");
            }
            _ => {}
        }
    }

    fn running(&self, busy: bool) {
        let progress = &self.imp().progress;
        progress.set_visible(busy);
        if busy {
            progress.set_fraction(0.0);
            progress.set_text(Some("Putting it back"));
        }
    }

    fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    fn build(&self) {
        let imp = self.imp();
        imp.progress.set_show_text(true);
        imp.progress.set_visible(false);
        imp.list.set_visible(false);

        imp.more.set_label("Show More");
        imp.more.set_halign(gtk::Align::Center);
        imp.more.add_css_class("pill");
        imp.more.set_visible(false);
        imp.more.connect_clicked(glib::clone!(
            #[weak(rename_to = history)]
            self,
            move |_| history.show_more()
        ));

        imp.empty.set_icon_name(Some("document-open-recent-symbolic"));
        imp.empty.set_title("Nothing Written Yet");
        imp.empty
            .set_description(Some("Every change applied to the photos is listed here."));
        imp.empty.add_css_class("compact");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&imp.progress);
        content.append(&imp.empty);
        content.append(&imp.list);
        content.append(&imp.more);
        let clamp = adw::Clamp::builder().maximum_size(720).child(&content).build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        imp.toasts.set_child(Some(&toolbar(&scrolled)));
        self.set_child(Some(&imp.toasts));
    }
}

/// One pass in the list: what ran, when, what it came to, and Take Back where that is possible.
fn row(pass: &Pass) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&pass.title))
        .subtitle(glib::markup_escape_text(&format!(
            "{} - {}",
            pass.started_at,
            came_to(pass)
        )))
        .activatable(true)
        .action_name("win.history-details")
        .action_target(&pass.id.to_variant())
        .build();
    if pass.can_take_back() {
        row.add_suffix(&take_back_button(pass.id));
    }
    row.add_suffix(
        &gtk::Image::builder()
            .icon_name("go-next-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
    );
    row
}

fn take_back_button(batch: i64) -> gtk::Button {
    let button = gtk::Button::builder()
        .label("Take Back")
        .valign(gtk::Align::Center)
        .action_name("win.undo-pass")
        .action_target(&batch.to_variant())
        .build();
    button.update_property(&[gtk::accessible::Property::Label("Take Back")]);
    button
}

/// What a pass came to, in a few words.
fn came_to(pass: &Pass) -> String {
    let photos = match pass.written {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    };
    match (pass.kind, pass.undone_by) {
        (Kind::Undo, _) => format!("{photos} put back"),
        (Kind::Write, Some(_)) => format!("{photos} changed, taken back"),
        (Kind::Write, None) => format!("{photos} changed"),
    }
}

fn pass_page(pass: &Pass, photos: &[Recorded]) -> adw::NavigationPage {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();

    let about = adw::PreferencesGroup::new();
    let mut facts = vec![("When", pass.started_at.clone()), ("Came to", came_to(pass))];
    if pass.changed_since > 0 {
        let (was, them) = match pass.changed_since {
            1 => ("was", "it as it is"),
            _ => ("were", "them as they are"),
        };
        facts.push((
            "Changed since",
            format!(
                "{} of its photos {was} changed again by a later pass. Taking it back leaves {them}.",
                pass.changed_since
            ),
        ));
    }
    for (title, value) in facts {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(glib::markup_escape_text(&value))
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        about.add(&row);
    }
    content.append(&about);

    if pass.can_take_back() {
        let button = take_back_button(pass.id);
        button.set_halign(gtk::Align::Center);
        button.add_css_class("pill");
        content.append(&button);
    }

    let list = adw::PreferencesGroup::builder().title("Photos").build();
    if photos.is_empty() {
        list.add(&adw::ActionRow::builder().title("No photo was touched").build());
    }
    for photo in photos {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&photo.rel_path))
            .subtitle(glib::markup_escape_text(&history::told(photo)))
            .subtitle_lines(0)
            .build();
        list.add(&row);
    }
    content.append(&list);

    let clamp = adw::Clamp::builder().maximum_size(720).child(&content).build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    adw::NavigationPage::builder()
        .title(&pass.title)
        .tag("pass")
        .child(&toolbar(&scrolled))
        .build()
}

/// A page with its own header, the way the preview has one: back button, title, no window buttons.
fn toolbar(content: &impl IsA<gtk::Widget>) -> adw::ToolbarView {
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(content));
    view
}

/// What a take-back came to, in a toast.
fn told(summary: &Summary) -> String {
    let mut parts = vec![format!("{} photos put back", summary.written)];
    for (count, name) in [(summary.refused, "left as they are"), (summary.failed, "failed")] {
        if count > 0 {
            parts.push(format!("{count} {name}"));
        }
    }
    if summary.cancelled {
        parts.push("stopped early".to_string());
    }
    parts.join(", ")
}
