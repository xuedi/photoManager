//! The page a tool that asks shows before it can change anything: one row per question, the
//! best offer beside it, and Confirm, the other offers and Leave Alone. A question about a place
//! also has Choose Another and Pick on Map, one about a camera's clock Enter a Shift, one about a
//! date Enter a Date. It draws the questions `core` hands over and knows neither the tool nor
//! what its questions are about: the words it uses for them are the tool's, and every offer
//! carries the answer Confirm puts in.
//!
//! Every answer goes into the tool's settings at once and is remembered, so leaving the page loses
//! nothing and the next run over another scope asks only what is new.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use photomanager_core::scope::Scope;
use photomanager_core::tools::{self, Answer, Kind, Located, Offer, Question};

use crate::library::Library;
use crate::panel::map_at;

/// The map a question is answered on, while it is open.
#[derive(Debug)]
pub struct Picking {
    pub question: String,
    dialog: adw::Dialog,
    mark: shumate::Marker,
    near: gtk::Label,
    use_point: gtk::Button,
    /// The pin the last click made, if the place data knows where it is.
    pub pin: Option<Answer>,
}

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
        pub picking: RefCell<Option<Picking>>,
        /// The question a shift or a date is being typed for, and the dialog it is typed in.
        pub typing: RefCell<Option<(String, adw::Dialog)>>,
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
    /// `forget`, `choose` for the place search, `map` for the map, `shift` or `date` to type one,
    /// or any answer as the settings write it.
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
            "map" => {
                self.pick_on_map(&asked);
                return true;
            }
            "shift" => {
                self.enter_shift(&asked);
                return true;
            }
            "date" => {
                self.enter_date(&asked);
                return true;
            }
            "forget" => None,
            "best" => match asked.offers.first() {
                Some(offer) => Some(offer.answer.clone()),
                None => return false,
            },
            other => match other.strip_prefix("offer:").map(str::parse::<usize>) {
                Some(Ok(index)) => match asked.offers.get(index) {
                    Some(offer) => Some(offer.answer.clone()),
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

    /// The tool's bulk button: every waiting question whose best offer is sure.
    pub fn answer_exact(&self) -> usize {
        let Some(tool) = self.key().and_then(|key| tools::find(&key)) else {
            return 0;
        };
        let questions = self.questions();
        let confirmed = questions.iter().filter(|question| question.confirmable()).count();
        if confirmed == 0 {
            return 0;
        }
        match tools::confirm_sure(tool, &questions, self.settings().as_deref()) {
            Ok(settings) => self.keep(settings),
            Err(why) => {
                tracing::error!(why, "the sure answers could not be confirmed");
                return 0;
            }
        }
        let wording = tool.wording();
        tracing::info!(confirmed, "sure answers confirmed");
        self.say(&match confirmed {
            1 => format!("1 {} confirmed", wording.one),
            count => format!("{count} {} confirmed", wording.many),
        });
        confirmed
    }

    /// The question the map is open for, and the pin it would answer with.
    pub fn picking(&self) -> Option<(String, Option<Answer>)> {
        self.imp()
            .picking
            .borrow()
            .as_ref()
            .map(|picking| (picking.question.clone(), picking.pin.clone()))
    }

    /// What a click on the map does: the mark moves there and the place it is in is named under
    /// it.
    pub fn pick_point(&self, lat: f64, lon: f64) {
        let Some(library) = self.imp().library.borrow().clone() else {
            return;
        };
        let mut picking = self.imp().picking.borrow_mut();
        let Some(picking) = picking.as_mut() else {
            return;
        };
        shumate::prelude::LocationExt::set_location(&picking.mark, lat, lon);
        picking.mark.set_visible(true);
        picking.pin = library.nearest(lat, lon).and_then(|at| Answer::pin(lat, lon, &at));
        picking.near.set_label(&match &picking.pin {
            Some(pin) => pin.tells(),
            None if library.counts().places == 0 => "There is no place data yet: get it on the dashboard".to_string(),
            None => "No place near this point".to_string(),
        });
        picking.use_point.set_sensitive(picking.pin.is_some());
    }

    /// Use This Point: the pin answers the question the map is open for, the way any answer
    /// does, so the tools are counted again.
    pub fn use_point(&self) -> bool {
        let Some(picking) = self.imp().picking.borrow_mut().take() else {
            return false;
        };
        picking.dialog.close();
        let (Some(pin), Some(key)) = (picking.pin, self.key()) else {
            return false;
        };
        let target = (key.as_str(), picking.question.as_str(), pin.written().as_str()).to_variant();
        match WidgetExt::activate_action(self, "win.answer", Some(&target)) {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%error, "the point could not be used");
                false
            }
        }
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
        let wording = tool.map(|tool| tool.wording()).unwrap_or_default();
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
        let sure = questions.iter().filter(|question| question.confirmable()).count();
        let any = !questions.is_empty();
        imp.loading.set_visible(busy && !any);
        imp.empty.set_visible(!busy && !any);
        imp.empty.set_description(Some(wording.unasked));
        imp.asked.set_title(wording.asked);
        imp.asked.set_visible(questions.iter().any(|question| !question.apart));
        imp.apart.set_visible(questions.iter().any(|question| question.apart));
        imp.exact_group.set_visible(any && !wording.confirm.is_empty());
        imp.exact.set_title(wording.confirm);
        imp.exact.set_sensitive(sure > 0);
        imp.exact_group.set_description(Some(&wording.sure(sure)));
        imp.title.set_subtitle(&match (busy, tool) {
            (true, _) => "Looking at the photos".to_string(),
            (false, Some(tool)) if waiting > 0 => tool.waiting(waiting),
            (false, _) => "Everything is answered".to_string(),
        });
        imp.preview.set_sensitive(!busy && tool.is_some());
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

    /// A map to drop a pin on for one question: centred on the pin it has or the best offer,
    /// the place the pin is in named under it, and Use This Point. The tiles come from
    /// OpenStreetMap, and only while it is open.
    fn pick_on_map(&self, question: &Question) {
        let pinned = match &question.answer {
            Some(Answer::Pin { lat, lon, .. }) => Some((*lat, *lon)),
            _ => None,
        };
        let (lat, lon, zoom) = match (pinned, question.offers.iter().find_map(Offer::place)) {
            (Some((lat, lon)), _) => (lat, lon, 14.0),
            (None, Some(place)) => (place.lat, place.lon, 11.0),
            (None, None) => (20.0, 0.0, 2.0),
        };
        let (map, mark) = map_at(lat, lon);
        map.set_height_request(360);
        map.set_vexpand(true);
        if let Some(viewport) = map.viewport() {
            viewport.set_zoom_level(zoom);
        }
        mark.set_visible(pinned.is_some());
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |gesture, _, x, y| {
                let Some(map) = gesture.widget().and_downcast::<shumate::SimpleMap>() else {
                    return;
                };
                if let Some(viewport) = map.viewport() {
                    let (lat, lon) = viewport.widget_coords_to_location(&map, x, y);
                    page.pick_point(lat, lon);
                }
            }
        ));
        map.add_controller(click);

        let hint = gtk::Label::builder()
            .label("Click the map where the photos were taken")
            .xalign(0.0)
            .wrap(true)
            .build();
        hint.add_css_class("dim-label");
        let near = gtk::Label::builder()
            .label(match &question.answer {
                Some(answer @ Answer::Pin { .. }) => answer.tells(),
                _ => "No point chosen yet".to_string(),
            })
            .xalign(0.0)
            .wrap(true)
            .build();
        near.update_property(&[gtk::accessible::Property::Label("Chosen Point")]);
        let use_point = gtk::Button::builder()
            .label("Use This Point")
            .halign(gtk::Align::Center)
            .sensitive(false)
            .build();
        use_point.add_css_class("pill");
        use_point.add_css_class("suggested-action");
        use_point.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.use_point();
            }
        ));

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&hint);
        content.append(&map);
        content.append(&near);
        content.append(&use_point);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(format!("Pick on Map: {}", question.title))
            .content_width(640)
            .content_height(560)
            .child(&view)
            .build();
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.imp().picking.borrow_mut().take();
            }
        ));
        *self.imp().picking.borrow_mut() = Some(Picking {
            question: question.key.clone(),
            dialog: dialog.clone(),
            mark,
            near,
            use_point,
            pin: None,
        });
        dialog.present(Some(self));
        tracing::info!(question = question.key, "map shown to pick a point");
    }

    /// The question a shift or a date is being typed for.
    pub fn typing(&self) -> Option<String> {
        self.imp()
            .typing
            .borrow()
            .as_ref()
            .map(|(question, _)| question.clone())
    }

    /// A shift typed for some cameras of a question, answered the way any answer is. What is
    /// wrong with it comes back in words.
    pub fn type_shift(&self, question: &str, typed: &[(String, String)]) -> Result<(), String> {
        let asked = self.question(question).ok_or("there is no such question")?;
        let answer = tools::shift_answer(&asked, typed)?;
        self.give(question, &answer)
    }

    /// A date typed for a question, answered the way any answer is.
    pub fn type_date(&self, question: &str, typed: &str) -> Result<(), String> {
        let answer = tools::date_answer(typed)?;
        self.give(question, &answer)
    }

    /// An answer made on this page goes through `win.answer` like every other, so the tools are
    /// counted again, and the dialog it was typed in closes.
    fn give(&self, question: &str, answer: &Answer) -> Result<(), String> {
        let key = self.key().ok_or("no tool is asking")?;
        let target = (key.as_str(), question, answer.written().as_str()).to_variant();
        WidgetExt::activate_action(self, "win.answer", Some(&target)).map_err(|error| error.to_string())?;
        let typed_in = self.imp().typing.borrow_mut().take();
        if let Some((_, dialog)) = typed_in {
            dialog.close();
        }
        Ok(())
    }

    /// One entry per camera of the event, each empty for a camera that was right.
    fn enter_shift(&self, question: &Question) {
        let hint = gtk::Label::builder()
            .label(
                "How far each camera's clock was off, such as -640d or +1y 2d 03:00. Leave a camera empty \
                 when it was right.",
            )
            .xalign(0.0)
            .wrap(true)
            .build();
        hint.add_css_class("dim-label");
        let answered: Vec<(String, String)> = match &question.answer {
            Some(Answer::Shift(moved)) => moved
                .iter()
                .map(|moved| (moved.camera.clone(), moved.by.written()))
                .collect(),
            _ => Vec::new(),
        };
        let mut groups: Vec<gtk::Widget> = vec![hint.upcast()];
        let mut entries = Vec::new();
        for evidence in &question.evidence {
            let group = adw::PreferencesGroup::builder()
                .title(glib::markup_escape_text(&evidence.camera))
                .description(glib::markup_escape_text(&evidence.facts()))
                .build();
            let entry = adw::EntryRow::builder()
                .title(format!("Shift for {}", evidence.camera))
                .build();
            if let Some((_, by)) = answered.iter().find(|(camera, _)| camera == &evidence.camera) {
                entry.set_text(by);
            }
            group.add(&entry);
            groups.push(group.upcast());
            entries.push((evidence.camera.clone(), entry, evidence.days_off.is_some()));
        }
        let first_off = entries
            .iter()
            .find(|(_, _, off)| *off)
            .map(|(_, entry, _)| entry.clone());
        let why = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        why.add_css_class("error");
        let button = gtk::Button::builder()
            .label("Use This Shift")
            .halign(gtk::Align::Center)
            .build();
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        let asked = question.key.clone();
        let use_it = glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            why,
            move || {
                let typed: Vec<(String, String)> = entries
                    .iter()
                    .map(|(camera, entry, _)| (camera.clone(), entry.text().to_string()))
                    .collect();
                if let Err(reason) = page.type_shift(&asked, &typed) {
                    why.set_label(&reason);
                    why.set_visible(true);
                }
            }
        );
        let use_it = Rc::new(use_it);
        button.connect_clicked(glib::clone!(
            #[strong]
            use_it,
            move |_| use_it()
        ));
        groups.push(why.upcast());
        groups.push(button.upcast());
        let children: Vec<&gtk::Widget> = groups.iter().collect();
        self.typing_dialog(&format!("Enter a Shift: {}", question.title), &question.key, &children);
        if let Some(entry) = first_off {
            entry.grab_focus();
        }
    }

    /// One entry for the date the event's photos without one are given.
    fn enter_date(&self, question: &Question) {
        let group = adw::PreferencesGroup::builder()
            .description("YYYY-MM-DD HH:MM:SS. Each photo after the first is given a second more.")
            .build();
        let entry = adw::EntryRow::builder().title("Date").build();
        match &question.answer {
            Some(Answer::Date(at)) => entry.set_text(at),
            _ => {
                if let Some(Answer::Date(at)) = question
                    .offers
                    .iter()
                    .map(|offer| &offer.answer)
                    .find(|answer| matches!(answer, Answer::Date(_)))
                {
                    entry.set_text(at);
                }
            }
        }
        group.add(&entry);
        let why = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        why.add_css_class("error");
        let button = gtk::Button::builder()
            .label("Use This Date")
            .halign(gtk::Align::Center)
            .build();
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        let asked = question.key.clone();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            why,
            #[weak]
            entry,
            move |_| {
                if let Err(reason) = page.type_date(&asked, entry.text().as_str()) {
                    why.set_label(&reason);
                    why.set_visible(true);
                }
            }
        ));
        self.typing_dialog(
            &format!("Enter a Date: {}", question.title),
            &question.key,
            &[group.upcast_ref(), why.upcast_ref(), button.upcast_ref()],
        );
    }

    fn typing_dialog(&self, title: &str, question: &str, children: &[&gtk::Widget]) {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        for child in children {
            content.append(*child);
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&content)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scrolled));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(480)
            .child(&view)
            .build();
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.imp().typing.borrow_mut().take();
            }
        ));
        *self.imp().typing.borrow_mut() = Some((question.to_string(), dialog.clone()));
        dialog.present(Some(self));
        tracing::info!(question, "shown to type an answer");
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
/// what is offered.
fn row(key: &str, question: &Question, has_places: bool) -> adw::ActionRow {
    let photos = match question.photos {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    };
    let best = question.offers.first();
    let state = match (&question.answer, best) {
        (Some(answer), _) => answer.tells(),
        (None, Some(offer)) => match (offer.place(), offer.located) {
            (Some(place), Some(1)) => format!("Best match {}, where 1 of its photos is", place.tells()),
            (Some(place), Some(count)) => format!("Best match {}, where {count} of its photos are", place.tells()),
            (Some(place), None) => format!("Best match {}, {}", place.tells(), percent(offer.confidence)),
            (None, _) => format!("Offered: {}", offer.words),
        },
        (None, None) if question.kind != Kind::Place => "Nothing to offer".to_string(),
        (None, None) if has_places => "No match in the place data".to_string(),
        (None, None) => "No place data yet".to_string(),
    };
    let subtitle = match &question.note {
        Some(note) => format!("{photos} - {state}\n{note}"),
        None => format!("{photos} - {state}"),
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&question.title))
        .subtitle(glib::markup_escape_text(&subtitle))
        .subtitle_lines(8)
        .build();
    let target = |answer: &str| (key, question.key.as_str(), answer).to_variant();

    if question.answer.as_ref().is_some_and(|answer| *answer != Answer::Leave) {
        let done = gtk::Image::from_icon_name("object-select-symbolic");
        done.update_property(&[gtk::accessible::Property::Label("Answered")]);
        row.add_prefix(&done);
    }
    let confirmed = matches!((&question.answer, best), (Some(answer), Some(offer)) if answer.same(&offer.answer));
    if let Some(offer) = best
        && !question.apart
        && !confirmed
    {
        let named = offer
            .place()
            .map(|place| place.name.clone())
            .unwrap_or_else(|| offer.words.clone());
        let confirm = gtk::Button::builder()
            .label("Confirm")
            .valign(gtk::Align::Center)
            .action_name("win.answer")
            .action_target(&target("best"))
            .tooltip_text(format!("Confirm {}", offer.words))
            .build();
        confirm.update_property(&[gtk::accessible::Property::Label(&format!(
            "Confirm {named} for {}",
            question.title
        ))]);
        row.add_suffix(&confirm);
    }

    let menu = gio::Menu::new();
    let item = |label: &str, answer: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some("win.answer"), Some(&target(answer)));
        menu.append_item(&item);
    };
    match question.kind {
        Kind::Place => {
            item("Choose Another…", "choose");
            item("Pick on Map…", "map");
        }
        kind => {
            for (index, offer) in question.offers.iter().enumerate().skip(1) {
                if question.answer.as_ref() != Some(&offer.answer) && offer.answer != Answer::Leave {
                    item(&offer.words, &format!("offer:{index}"));
                }
            }
            match kind {
                Kind::Shift => item("Enter a Shift…", "shift"),
                Kind::Date => item("Enter a Date…", "date"),
                _ => {}
            }
        }
    }
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
