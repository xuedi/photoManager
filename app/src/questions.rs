//! The page a tool that asks shows before it can change anything: one row per question, the
//! place data's best offer beside it, and Confirm, Choose Another and Leave Alone. It draws the
//! questions `core` hands over and knows neither the tool nor what its questions are about.
//!
//! Every answer goes into the tool's settings at once and is remembered, so leaving the page loses
//! nothing and the next run over another scope asks only what is new.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use photomanager_core::scope::Scope;
use photomanager_core::tools::{self, Answer, Located, Question};

use crate::library::Library;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct Questions {
        pub toasts: adw::ToastOverlay,
        pub title: adw::WindowTitle,
        pub preview: gtk::Button,
        pub loading: adw::StatusPage,
        pub empty: adw::StatusPage,
        pub exact_group: adw::PreferencesGroup,
        pub exact: adw::ButtonRow,
        pub asked: adw::PreferencesGroup,
        pub apart: adw::PreferencesGroup,
        pub rows: RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub key: RefCell<Option<String>>,
        pub settings: RefCell<Option<String>>,
        pub questions: RefCell<Vec<Question>>,
        /// Which asking is the newest, so one that arrives late is dropped.
        pub asking: Cell<u64>,
        pub busy: Cell<bool>,
        pub toast: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Questions {
        const NAME: &'static str = "PmQuestions";
        type Type = super::Questions;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for Questions {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for Questions {}
    impl BinImpl for Questions {}
}

glib::wrapper! {
    pub struct Questions(ObjectSubclass<imp::Questions>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Questions {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl Questions {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        *self.imp().library.borrow_mut() = library;
    }

    /// The tool whose questions are shown.
    pub fn key(&self) -> Option<String> {
        self.imp().key.borrow().clone()
    }

    /// Its settings with every answer given so far, as text.
    pub fn settings(&self) -> Option<String> {
        self.imp().settings.borrow().clone()
    }

    pub fn questions(&self) -> Vec<Question> {
        self.imp().questions.borrow().clone()
    }

    /// Whether the questions are still being asked.
    pub fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    /// Asks a tool's questions about the scope, with the answers it was last given.
    pub fn open(&self, key: &str, scope: &Scope) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let Some(tool) = tools::find(key) else {
            return;
        };
        if imp.key.borrow().as_deref() != Some(key) {
            imp.questions.borrow_mut().clear();
            self.show_rows();
        }
        *imp.key.borrow_mut() = Some(key.to_string());
        *imp.settings.borrow_mut() = library.tool_settings(key);
        imp.exact.set_action_target_value(Some(&key.to_variant()));
        imp.exact.set_action_name(Some("win.answer-exact"));
        imp.title.set_title(tool.title());
        self.ask(scope);
    }

    /// Asks again: after a scan, or when the scope changed.
    pub fn ask(&self, scope: &Scope) {
        let imp = self.imp();
        let (Some(library), Some(key)) = (imp.library.borrow().clone(), self.key()) else {
            return;
        };
        let asking = imp.asking.get() + 1;
        imp.asking.set(asking);
        imp.busy.set(true);
        self.show_rows();

        let page = self.downgrade();
        library.questions(&key, self.settings(), scope, move |asked| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let imp = page.imp();
            if imp.asking.get() != asking {
                return;
            }
            imp.busy.set(false);
            match asked {
                Ok(questions) => *imp.questions.borrow_mut() = questions,
                Err(why) => {
                    tracing::error!(why, "the questions could not be asked");
                    page.say(&format!("The questions could not be asked: {why}"));
                }
            }
            page.show_rows();
        });
    }

    /// One answer, in the words the `win.answer` action takes: `best`, `offer:N`, `leave`,
    /// `forget`, `choose` for the place search, or a place as the settings write it.
    pub fn answer(&self, question: &str, answer: &str) -> bool {
        let Some(asked) = self.question(question) else {
            tracing::warn!(question, "no such question");
            return false;
        };
        let given = match answer {
            "choose" => {
                self.choose(&asked);
                return true;
            }
            "forget" => None,
            "best" => match asked.offers.first() {
                Some(offer) => Some(Answer::Place(offer.place.clone())),
                None => return false,
            },
            other => match other.strip_prefix("offer:").map(str::parse::<usize>) {
                Some(Ok(index)) => match asked.offers.get(index) {
                    Some(offer) => Some(Answer::Place(offer.place.clone())),
                    None => return false,
                },
                Some(Err(_)) => return false,
                None => match Answer::read(other) {
                    Ok(answer) => Some(answer),
                    Err(why) => {
                        tracing::warn!(why, "not an answer");
                        return false;
                    }
                },
            },
        };
        let Some(tool) = self.key().and_then(|key| tools::find(&key)) else {
            return false;
        };
        let told = given.as_ref().map(Answer::tells);
        match tool.answer(self.settings().as_deref(), question, given) {
            Ok(settings) => self.keep(settings),
            Err(why) => {
                tracing::error!(why, "the answer could not be kept");
                return false;
            }
        }
        tracing::info!(
            question,
            answer = told.as_deref().unwrap_or("asked again"),
            "question answered"
        );
        true
    }

    /// Confirm Exact Matches: every waiting question whose best offer is an exact name the place
    /// data is sure of.
    pub fn answer_exact(&self) -> usize {
        let Some(tool) = self.key().and_then(|key| tools::find(&key)) else {
            return 0;
        };
        let questions = self.questions();
        let confirmed = questions
            .iter()
            .filter(|question| !question.apart && question.waits() && question.exact().is_some())
            .count();
        if confirmed == 0 {
            return 0;
        }
        match tools::confirm_exact(tool, &questions, self.settings().as_deref()) {
            Ok(settings) => self.keep(settings),
            Err(why) => {
                tracing::error!(why, "the exact matches could not be confirmed");
                return 0;
            }
        }
        tracing::info!(confirmed, "exact matches confirmed");
        self.say(&match confirmed {
            1 => "1 exact match confirmed".to_string(),
            count => format!("{count} exact matches confirmed"),
        });
        confirmed
    }

    /// The answers are the tool's settings: kept at once, and read back onto the questions on
    /// screen without asking the library again.
    fn keep(&self, settings: String) {
        let imp = self.imp();
        let (Some(library), Some(key)) = (imp.library.borrow().clone(), self.key()) else {
            return;
        };
        library.remember_tool_settings(&key, &settings);
        if let Some(tool) = tools::find(&key)
            && let Err(why) = tools::answer_again(tool, &mut imp.questions.borrow_mut(), Some(&settings))
        {
            tracing::error!(why, "the answers could not be read back");
        }
        *imp.settings.borrow_mut() = Some(settings);
        self.show_rows();
    }

    fn question(&self, key: &str) -> Option<Question> {
        self.imp()
            .questions
            .borrow()
            .iter()
            .find(|question| question.key == key)
            .cloned()
    }

    fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    fn show_rows(&self) {
        let imp = self.imp();
        for (group, row) in imp.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let key = self.key().unwrap_or_default();
        let tool = tools::find(&key);
        let questions = imp.questions.borrow();
        let has_places = imp
            .library
            .borrow()
            .as_ref()
            .is_some_and(|library| library.counts().places > 0);

        for question in questions.iter() {
            let group = match question.apart {
                true => &imp.apart,
                false => &imp.asked,
            };
            let row = row(&key, question, has_places);
            group.add(&row);
            imp.rows.borrow_mut().push((group.clone(), row));
        }

        let busy = imp.busy.get();
        let waiting = questions.iter().filter(|question| question.waits()).count();
        let exact = questions
            .iter()
            .filter(|question| !question.apart && question.waits() && question.exact().is_some())
            .count();
        let any = !questions.is_empty();
        imp.loading.set_visible(busy && !any);
        imp.empty.set_visible(!busy && !any);
        imp.asked.set_visible(questions.iter().any(|question| !question.apart));
        imp.apart.set_visible(questions.iter().any(|question| question.apart));
        imp.exact_group.set_visible(any);
        imp.exact.set_sensitive(exact > 0);
        imp.exact_group.set_description(Some(&match exact {
            0 => "Nothing waits that matches a place by its exact name. Every other tag is one click.".to_string(),
            1 => "1 tag matches a place by its exact name. Every other tag is one click.".to_string(),
            count => format!("{count} tags match a place by its exact name. Every other tag is one click."),
        }));
        imp.title.set_subtitle(&match (busy, tool) {
            (true, _) => "Looking at the photos".to_string(),
            (false, Some(tool)) if waiting > 0 => tool.waiting(waiting),
            (false, _) => "Everything is answered".to_string(),
        });
        imp.preview.set_sensitive(any);
    }

    /// The place search for one question, prefilled with what it asks about.
    fn choose(&self, question: &Question) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let key = self.key().unwrap_or_default();
        let dialog = adw::Dialog::builder()
            .title("Choose a Place")
            .content_width(420)
            .content_height(520)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build();
        list.add_css_class("boxed-list");
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Places")
            .text(question.title.as_str())
            .hexpand(true)
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search Places")]);

        let fill = glib::clone!(
            #[weak]
            list,
            #[weak]
            dialog,
            #[strong]
            library,
            #[strong]
            key,
            #[strong(rename_to = asked)]
            question.key,
            move |text: &str| {
                list.remove_all();
                let found = library.find_place(text);
                if found.is_empty() {
                    let none = adw::ActionRow::builder()
                        .title(match library.counts().places {
                            0 => "There is no place data yet: get it on the dashboard",
                            _ => "Nothing by that name in the place data",
                        })
                        .build();
                    list.append(&none);
                }
                for candidate in found {
                    let place = Located::of(&candidate.place);
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&place.tells()))
                        .subtitle(format!("{} match", percent(candidate.confidence)))
                        .activatable(true)
                        .build();
                    let target = (key.as_str(), asked.as_str(), Answer::Place(place).written().as_str()).to_variant();
                    row.connect_activated(glib::clone!(
                        #[weak]
                        dialog,
                        move |row| {
                            if let Err(error) = WidgetExt::activate_action(row, "win.answer", Some(&target)) {
                                tracing::error!(%error, "the place could not be chosen");
                            }
                            dialog.close();
                        }
                    ));
                    list.append(&row);
                }
            }
        );
        fill(question.title.as_str());
        search.connect_search_changed(move |search| fill(search.text().as_str()));

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&search);
        content.append(&list);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scrolled));
        dialog.set_child(Some(&view));
        dialog.present(Some(self));
    }

    fn build(&self) {
        let imp = self.imp();

        imp.preview.set_label("Preview");
        imp.preview.add_css_class("suggested-action");
        imp.preview.set_action_name(Some("win.preview-answers"));
        imp.preview.set_sensitive(false);
        imp.preview
            .update_property(&[gtk::accessible::Property::Label("Preview")]);
        imp.title.set_title("Questions");

        imp.loading.set_title("Looking at the Photos");
        imp.loading.set_child(Some(&adw::Spinner::new()));
        imp.loading.add_css_class("compact");
        imp.loading.set_visible(false);

        imp.empty.set_icon_name(Some("object-select-symbolic"));
        imp.empty.set_title("Nothing to Ask");
        imp.empty.set_description(Some(
            "No photo of the scope is waiting for this tool. Choose another scope on the Tools page.",
        ));
        imp.empty.add_css_class("compact");
        imp.empty.set_visible(false);

        imp.exact.set_title("Confirm Exact Matches");
        imp.exact.set_start_icon_name(Some("object-select-symbolic"));
        imp.exact_group.add(&imp.exact);
        imp.exact_group.set_visible(false);

        imp.asked.set_title("Tags");
        imp.asked.set_visible(false);
        imp.apart.set_title("Countries");
        imp.apart.set_description(Some(
            "Left alone: a country is not a place anyone took a photo. Answer one by hand where it \
             really is one town.",
        ));
        imp.apart.set_visible(false);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&imp.loading);
        content.append(&imp.empty);
        content.append(&imp.exact_group);
        content.append(&imp.asked);
        content.append(&imp.apart);
        let clamp = adw::Clamp::builder().maximum_size(720).child(&content).build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .title_widget(&imp.title)
            .build();
        header.pack_end(&imp.preview);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&scrolled));
        imp.toasts.set_child(Some(&view));
        self.set_child(Some(&imp.toasts));
    }
}

/// One question: what it asks about, how many photos wait on it, and what it is answered with or
/// what the place data offers.
fn row(key: &str, question: &Question, has_places: bool) -> adw::ActionRow {
    let photos = match question.photos {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    };
    let best = question.offers.first();
    let state = match (&question.answer, best) {
        (Some(answer), _) => answer.tells(),
        (None, Some(offer)) => format!("Best match {}, {}", offer.place.tells(), percent(offer.confidence)),
        (None, None) if has_places => "No match in the place data".to_string(),
        (None, None) => "No place data yet".to_string(),
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&question.title))
        .subtitle(glib::markup_escape_text(&format!("{photos} - {state}")))
        .subtitle_lines(3)
        .build();
    let target = |answer: &str| (key, question.key.as_str(), answer).to_variant();

    if let Some(Answer::Place(_)) = &question.answer {
        let done = gtk::Image::from_icon_name("object-select-symbolic");
        done.update_property(&[gtk::accessible::Property::Label("Answered")]);
        row.add_prefix(&done);
    }
    let confirmed =
        matches!((&question.answer, best), (Some(Answer::Place(place)), Some(offer)) if place.id == offer.place.id);
    if let Some(offer) = best
        && !question.apart
        && !confirmed
    {
        let confirm = gtk::Button::builder()
            .label("Confirm")
            .valign(gtk::Align::Center)
            .action_name("win.answer")
            .action_target(&target("best"))
            .tooltip_text(format!("Confirm {}", offer.place.tells()))
            .build();
        confirm.update_property(&[gtk::accessible::Property::Label(&format!(
            "Confirm {} for {}",
            offer.place.name, question.title
        ))]);
        row.add_suffix(&confirm);
    }

    let menu = gio::Menu::new();
    let item = |label: &str, answer: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some("win.answer"), Some(&target(answer)));
        menu.append_item(&item);
    };
    item("Choose Another…", "choose");
    if question.answer != Some(Answer::Leave) {
        item("Leave Alone", "leave");
    }
    if question.answer.is_some() && !(question.apart && question.answer == Some(Answer::Leave)) {
        item("Ask Again", "forget");
    }
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .valign(gtk::Align::Center)
        .menu_model(&menu)
        .tooltip_text("More Answers")
        .build();
    more.add_css_class("flat");
    more.update_property(&[gtk::accessible::Property::Label(&format!(
        "More Answers for {}",
        question.title
    ))]);
    row.add_suffix(&more);
    row
}

fn percent(confidence: f64) -> String {
    format!("{:.0} %", confidence * 100.0)
}
