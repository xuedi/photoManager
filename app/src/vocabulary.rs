//! The page the tag vocabulary opens instead of questions, because a tree is not a list of
//! questions: the suggestions on top, the rules so far in order, what becomes of the generated
//! tags, and the tree as it will be after the rules, each tag with Rename or Move, Merge Into and
//! Delete.
//!
//! Every change is a rule in the tool's settings, kept at once, so leaving the page loses
//! nothing. The tree and its counts are worked out off the main thread from the cache.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use photomanager_core::browse::Tag;
use photomanager_core::tags::{Rule, within};
use photomanager_core::tools::Settings;
use photomanager_core::tools::tag_vocabulary::{Generated, Overview, Vocabulary};

use crate::library::Library;

/// A tag that takes more photos than this away asks before it is kept.
const MANY: i64 = 100;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct VocabularyPage {
        pub toasts: adw::ToastOverlay,
        pub title: adw::WindowTitle,
        pub preview: gtk::Button,
        pub loading: adw::StatusPage,
        pub suggested: adw::PreferencesGroup,
        pub rules: adw::PreferencesGroup,
        pub generated: adw::ComboRow,
        pub filling: Cell<bool>,
        pub search: gtk::SearchEntry,
        pub tree_group: adw::PreferencesGroup,
        pub tree: gtk::ListBox,
        pub found: gtk::ListBox,
        pub rows: RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>,
        pub library: RefCell<Option<Rc<Library>>>,
        pub key: RefCell<Option<String>>,
        pub vocabulary: RefCell<Vocabulary>,
        pub overview: RefCell<Option<Overview>>,
        /// Which look is the newest, so one that arrives late is dropped.
        pub asking: Cell<u64>,
        pub busy: Cell<bool>,
        pub toast: RefCell<String>,
        /// Why the last rule was not kept.
        pub refused: RefCell<Option<String>>,
        /// The tag a rename or a merge is being chosen for, and the dialog it is chosen in.
        pub editing: RefCell<Option<(String, adw::Dialog)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VocabularyPage {
        const NAME: &'static str = "PmVocabulary";
        type Type = super::VocabularyPage;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for VocabularyPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for VocabularyPage {}
    impl BinImpl for VocabularyPage {}
}

glib::wrapper! {
    pub struct VocabularyPage(ObjectSubclass<imp::VocabularyPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for VocabularyPage {
    fn default() -> Self {
        glib::Object::builder().build()
    }
}

impl VocabularyPage {
    pub fn set_library(&self, library: Option<Rc<Library>>) {
        *self.imp().library.borrow_mut() = library;
    }

    /// The tool whose vocabulary is shown.
    pub fn key(&self) -> Option<String> {
        self.imp().key.borrow().clone()
    }

    /// Its settings, as text.
    pub fn settings(&self) -> Option<String> {
        self.key().map(|_| self.imp().vocabulary.borrow().written())
    }

    pub fn vocabulary(&self) -> Vocabulary {
        self.imp().vocabulary.borrow().clone()
    }

    pub fn overview(&self) -> Option<Overview> {
        self.imp().overview.borrow().clone()
    }

    pub fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    pub fn toast(&self) -> String {
        self.imp().toast.borrow().clone()
    }

    pub fn refused(&self) -> Option<String> {
        self.imp().refused.borrow().clone()
    }

    /// The tag a rename or a merge is being chosen for.
    pub fn editing(&self) -> Option<String> {
        self.imp().editing.borrow().as_ref().map(|(path, _)| path.clone())
    }

    /// Shows a tool's vocabulary with the settings it was last given.
    pub fn open(&self, key: &str) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        if imp.key.borrow().as_deref() != Some(key) {
            *imp.overview.borrow_mut() = None;
        }
        *imp.key.borrow_mut() = Some(key.to_string());
        let remembered = library.tool_settings(key).unwrap_or_default();
        *imp.vocabulary.borrow_mut() = Vocabulary::read(&remembered).unwrap_or_else(|why| {
            tracing::error!(why, "the tag vocabulary could not be read, starting afresh");
            Vocabulary::default()
        });
        self.look();
    }

    /// Works the tree out again: after every rule, and after a scan.
    pub fn look(&self) {
        let imp = self.imp();
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        if self.key().is_none() {
            return;
        }
        let asking = imp.asking.get() + 1;
        imp.asking.set(asking);
        imp.busy.set(true);
        self.show();

        let page = self.downgrade();
        library.vocabulary(self.vocabulary(), move |looked| {
            let Some(page) = page.upgrade() else {
                return;
            };
            let imp = page.imp();
            if imp.asking.get() != asking {
                return;
            }
            imp.busy.set(false);
            match looked {
                Ok(overview) => *imp.overview.borrow_mut() = Some(overview),
                Err(why) => {
                    tracing::error!(why, "the tags could not be looked at");
                    page.say(&format!("The tags could not be looked at: {why}"));
                }
            }
            page.show();
        });
    }

    /// One rule at the end, in the words it is written in. What is wrong with it comes back.
    pub fn add_rule(&self, written: &str) -> Result<(), String> {
        let added = Rule::read(written).and_then(|rule| {
            let mut vocabulary = self.vocabulary();
            vocabulary.rules.add(rule.clone())?;
            Ok((vocabulary, rule))
        });
        match added {
            Ok((vocabulary, rule)) => {
                *self.imp().refused.borrow_mut() = None;
                tracing::info!(rule = rule.written(), "tag rule kept");
                self.keep(vocabulary);
                self.say(&format!("{} is a rule now", rule.tells()));
                Ok(())
            }
            Err(why) => {
                tracing::warn!(why, "tag rule refused");
                *self.imp().refused.borrow_mut() = Some(why.clone());
                self.say(&format!("Not kept: {why}"));
                Err(why)
            }
        }
    }

    /// Takes one rule out. The ones after it that no longer make sense without it go too, and
    /// the page says so.
    pub fn forget_rule(&self, index: usize) -> bool {
        let mut rules = self.vocabulary().rules.0;
        if index >= rules.len() {
            return false;
        }
        let gone = rules.remove(index);
        let mut vocabulary = self.vocabulary();
        vocabulary.rules.0.clear();
        let mut dropped = 0;
        for rule in rules {
            if vocabulary.rules.add(rule).is_err() {
                dropped += 1;
            }
        }
        tracing::info!(rule = gone.written(), dropped, "tag rule taken out");
        self.keep(vocabulary);
        self.say(&match dropped {
            0 => format!("{} is no rule any more", gone.tells()),
            1 => format!("{} is no rule any more, nor is 1 that followed from it", gone.tells()),
            count => format!(
                "{} is no rule any more, nor are {count} that followed from it",
                gone.tells()
            ),
        });
        true
    }

    /// Confirm or Leave Alone on one suggestion, or Suggest Again for one left alone.
    pub fn suggestion(&self, key: &str, answer: &str) -> bool {
        let mut vocabulary = self.vocabulary();
        match answer {
            "confirm" => {
                let Some(suggestion) = self
                    .overview()
                    .and_then(|overview| overview.suggestions.into_iter().find(|one| one.key == key))
                else {
                    tracing::warn!(suggestion = key, "no such suggestion");
                    return false;
                };
                if let Err(why) = vocabulary.confirm(&suggestion) {
                    self.say(&format!("Not kept: {why}"));
                    return false;
                }
                self.say(&format!("{}: {}", suggestion.title, suggestion.offer));
            }
            "leave" => {
                vocabulary.left.insert(key.to_string());
            }
            "again" => {
                vocabulary.left.remove(key);
            }
            _ => return false,
        }
        tracing::info!(suggestion = key, answer, "tag suggestion answered");
        self.keep(vocabulary);
        true
    }

    pub fn set_generated(&self, key: &str) -> bool {
        let Some(generated) = Generated::named(key) else {
            return false;
        };
        let mut vocabulary = self.vocabulary();
        if vocabulary.generated == generated {
            return true;
        }
        vocabulary.generated = generated;
        tracing::info!(generated = key, "generated tags chosen");
        self.keep(vocabulary);
        true
    }

    /// The settings are kept at once and the tree is worked out again.
    fn keep(&self, vocabulary: Vocabulary) {
        let imp = self.imp();
        if let (Some(library), Some(key)) = (imp.library.borrow().clone(), self.key()) {
            library.remember_tool_settings(&key, &vocabulary.written());
        }
        *imp.vocabulary.borrow_mut() = vocabulary;
        self.look();
    }

    fn say(&self, text: &str) {
        *self.imp().toast.borrow_mut() = text.to_string();
        self.imp().toasts.add_toast(adw::Toast::new(text));
    }

    fn show(&self) {
        let imp = self.imp();
        for (group, row) in imp.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        let vocabulary = self.vocabulary();
        let overview = imp.overview.borrow().clone();
        let busy = imp.busy.get();
        imp.loading.set_visible(busy && overview.is_none());

        imp.filling.set(true);
        let at = Generated::ALL
            .iter()
            .position(|generated| *generated == vocabulary.generated)
            .unwrap_or_default();
        imp.generated.set_selected(at as u32);
        imp.filling.set(false);

        let Some(overview) = overview else {
            imp.suggested.set_visible(false);
            imp.rules.set_visible(false);
            imp.tree_group.set_visible(false);
            imp.title.set_subtitle("Looking at the tags");
            imp.preview.set_sensitive(false);
            return;
        };

        for suggestion in &overview.suggestions {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&suggestion.title))
                .subtitle(glib::markup_escape_text(&format!(
                    "{} - {}",
                    photos(suggestion.photos),
                    suggestion.offer
                )))
                .build();
            let target = |answer: &str| (suggestion.key.as_str(), answer).to_variant();
            let confirm = gtk::Button::builder()
                .label("Confirm")
                .valign(gtk::Align::Center)
                .action_name("win.tag-suggestion")
                .action_target(&target("confirm"))
                .tooltip_text(suggestion.offer.as_str())
                .build();
            confirm.update_property(&[gtk::accessible::Property::Label(&format!(
                "Confirm {} for {}",
                suggestion.offer, suggestion.title
            ))]);
            row.add_suffix(&confirm);
            let menu = gio::Menu::new();
            let item = gio::MenuItem::new(Some("Leave Alone"), None);
            item.set_action_and_target_value(Some("win.tag-suggestion"), Some(&target("leave")));
            menu.append_item(&item);
            row.add_suffix(&more(&menu, &format!("More for {}", suggestion.title)));
            imp.suggested.add(&row);
            imp.rows.borrow_mut().push((imp.suggested.clone(), row.upcast()));
        }
        imp.suggested.set_visible(!overview.suggestions.is_empty());

        for (index, (rule, touched)) in overview.rules.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&rule.tells()))
                .subtitle(format!("Changes {}", photos(*touched as i64)))
                .build();
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .action_name("win.tag-forget-rule")
                .action_target(&(index as i32).to_variant())
                .tooltip_text("Remove Rule")
                .build();
            remove.add_css_class("flat");
            remove.update_property(&[gtk::accessible::Property::Label(&format!("Remove {}", rule.tells()))]);
            row.add_suffix(&remove);
            imp.rules.add(&row);
            imp.rows.borrow_mut().push((imp.rules.clone(), row.upcast()));
        }
        imp.rules.set_visible(true);
        imp.rules.set_description(Some(match overview.rules.is_empty() {
            true => "No rules yet. Rename, merge or delete a tag in the tree below, or confirm a suggestion.",
            false => "Applied in order to every photo, on this run and every later one",
        }));

        imp.tree.remove_all();
        for tag in overview.tree.tree() {
            imp.tree.append(&node(&tag));
        }
        imp.tree_group.set_visible(true);
        self.search();

        imp.title.set_subtitle(&match (busy, overview.rules.len()) {
            (true, _) => "Looking at the tags".to_string(),
            (false, 0) => "No rules yet".to_string(),
            (false, 1) => "1 rule".to_string(),
            (false, count) => format!("{count} rules"),
        });
        imp.preview.set_sensitive(!busy);
    }

    /// While something is typed the tree is a flat list of the tags whose path holds it.
    fn search(&self) {
        let imp = self.imp();
        let wanted = imp.search.text().to_lowercase();
        imp.found.remove_all();
        let searching = !wanted.is_empty();
        imp.tree.set_visible(!searching);
        imp.found.set_visible(searching);
        if !searching {
            return;
        }
        let Some(overview) = imp.overview.borrow().clone() else {
            return;
        };
        let mut any = false;
        for (path, count) in overview.tree.nodes() {
            if path.to_lowercase().contains(&wanted) {
                imp.found.append(&leaf_row(path, path, count));
                any = true;
            }
        }
        if !any {
            imp.found
                .append(&adw::ActionRow::builder().title("No tag holds that").build());
        }
    }

    /// Rename or Move: the whole path to edit, so a leaf is renamed and a branch moved alike.
    pub fn rename(&self, path: &str) {
        let hint = gtk::Label::builder()
            .label(
                "A new name, or a whole new path to move the tag there with everything below it. A tag \
                 that is already there is merged into.",
            )
            .xalign(0.0)
            .wrap(true)
            .build();
        hint.add_css_class("dim-label");
        let group = adw::PreferencesGroup::new();
        let entry = adw::EntryRow::builder().title("New Path").text(path).build();
        group.add(&entry);
        let why = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        why.add_css_class("error");
        let button = gtk::Button::builder()
            .label("Rename")
            .halign(gtk::Align::Center)
            .build();
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        let from = path.to_string();
        let use_it = glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            why,
            #[weak]
            entry,
            move || {
                let written = format!("rename {from} -> {}", entry.text());
                match page.add_rule(&written) {
                    Ok(()) => page.close_editing(),
                    Err(reason) => {
                        why.set_label(&reason);
                        why.set_visible(true);
                    }
                }
            }
        );
        let use_it = Rc::new(use_it);
        button.connect_clicked(glib::clone!(
            #[strong]
            use_it,
            move |_| use_it()
        ));
        entry.connect_entry_activated(move |_| use_it());
        self.editing_dialog(
            &format!("Rename or Move: {path}"),
            path,
            &[
                hint.upcast_ref(),
                group.upcast_ref(),
                why.upcast_ref(),
                button.upcast_ref(),
            ],
        );
        entry.grab_focus();
    }

    /// Merge Into: a search over the tree for the tag it goes into.
    pub fn merge(&self, path: &str) {
        let Some(overview) = self.overview() else {
            return;
        };
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build();
        list.add_css_class("boxed-list");
        let searched: Rc<RefCell<Vec<(gtk::ListBoxRow, String)>>> = Rc::default();
        for (target, count) in overview.tree.nodes() {
            if within(target, path) || within(path, target) {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(target))
                .subtitle(photos(count))
                .activatable(true)
                .build();
            let written = format!("rename {path} -> {target}");
            row.connect_activated(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    if page.add_rule(&written).is_ok() {
                        page.close_editing();
                    }
                }
            ));
            list.append(&row);
            searched.borrow_mut().push((row.upcast(), target.to_lowercase()));
        }
        let wanted: Rc<RefCell<String>> = Rc::default();
        list.set_filter_func(glib::clone!(
            #[strong]
            wanted,
            move |row| {
                let wanted = wanted.borrow();
                wanted.is_empty()
                    || searched
                        .borrow()
                        .iter()
                        .any(|(each, words)| each == row && words.contains(wanted.as_str()))
            }
        ));
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Tags")
            .hexpand(true)
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search Tags")]);
        search.connect_search_changed(glib::clone!(
            #[weak]
            list,
            move |search| {
                *wanted.borrow_mut() = search.text().to_lowercase();
                list.invalidate_filter();
            }
        ));
        self.editing_dialog(
            &format!("Merge Into: {path}"),
            path,
            &[search.upcast_ref(), list.upcast_ref()],
        );
        search.grab_focus();
    }

    /// Delete: at once for a few photos, after a question for many.
    pub fn delete(&self, path: &str) {
        let count = self
            .overview()
            .and_then(|overview| overview.tree.count(path))
            .unwrap_or_default();
        let written = format!("delete {path}");
        if count <= MANY {
            let _ = self.add_rule(&written);
            return;
        }
        let alert = adw::AlertDialog::builder()
            .heading(format!("Delete {path}?"))
            .body(format!(
                "It is taken from {}, with every tag below it. Nothing is written until the preview is applied.",
                photos(count)
            ))
            .build();
        alert.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");
        alert.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_, response| {
                    if response == "delete" {
                        let _ = page.add_rule(&written);
                    }
                }
            ),
        );
        alert.present(Some(self));
    }

    fn close_editing(&self) {
        let open = self.imp().editing.borrow_mut().take();
        if let Some((_, dialog)) = open {
            dialog.close();
        }
    }

    fn editing_dialog(&self, title: &str, path: &str, children: &[&gtk::Widget]) {
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
            .content_height(420)
            .child(&view)
            .build();
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.imp().editing.borrow_mut().take();
            }
        ));
        *self.imp().editing.borrow_mut() = Some((path.to_string(), dialog.clone()));
        dialog.present(Some(self));
        tracing::info!(tag = path, "shown to edit a tag");
    }

    fn build(&self) {
        let imp = self.imp();

        imp.preview.set_label("Preview");
        imp.preview.add_css_class("suggested-action");
        imp.preview.set_action_name(Some("win.preview-tags"));
        imp.preview.set_sensitive(false);
        imp.title.set_title("Tag Vocabulary");

        imp.loading.set_title("Looking at the Tags");
        imp.loading.set_child(Some(&adw::Spinner::new()));
        imp.loading.add_css_class("compact");
        imp.loading.set_visible(false);

        imp.suggested.set_title("Suggestions");
        imp.suggested
            .set_description(Some("What the tree already shows. Confirm makes it a rule."));
        imp.suggested.set_visible(false);

        imp.rules.set_title("Rules");
        imp.rules.set_visible(false);

        let choices: Vec<&str> = Generated::ALL.iter().map(|generated| generated.tells()).collect();
        imp.generated.set_title("Generated Tags");
        imp.generated
            .set_subtitle("The year, place and event tags: made from the date, the place words and the folder, dropped, or left as they are");
        imp.generated.set_subtitle_lines(3);
        imp.generated.set_model(Some(&gtk::StringList::new(&choices)));
        imp.generated.connect_selected_notify(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |row| {
                if page.imp().filling.get() {
                    return;
                }
                if let Some(generated) = Generated::ALL.get(row.selected() as usize)
                    && let Err(error) =
                        WidgetExt::activate_action(&page, "win.tag-generated", Some(&generated.key().to_variant()))
                {
                    tracing::error!(%error, "the generated tags could not be chosen");
                }
            }
        ));
        let generated_group = adw::PreferencesGroup::new();
        generated_group.add(&imp.generated);

        imp.search.set_placeholder_text(Some("Search Tags"));
        imp.search
            .update_property(&[gtk::accessible::Property::Label("Search Tags")]);
        imp.search.connect_search_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.search()
        ));
        for list in [&imp.tree, &imp.found] {
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("boxed-list");
        }
        imp.found.set_visible(false);
        let lists = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        lists.append(&imp.search);
        lists.append(&imp.tree);
        lists.append(&imp.found);
        imp.tree_group.set_title("Tags");
        imp.tree_group.set_description(Some(
            "The tree after the rules, each tag with the photos that carry it or a tag below it",
        ));
        imp.tree_group.add(&lists);
        imp.tree_group.set_visible(false);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&imp.loading);
        content.append(&imp.suggested);
        content.append(&imp.rules);
        content.append(&generated_group);
        content.append(&imp.tree_group);
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

/// A tag of the tree: a row that opens to its children where it has any.
fn node(tag: &Tag) -> gtk::Widget {
    if tag.children.is_empty() {
        return leaf_row(&tag.name, &tag.path, tag.photos).upcast();
    }
    let row = adw::ExpanderRow::builder()
        .title(glib::markup_escape_text(&tag.name))
        .subtitle(photos(tag.photos))
        .build();
    row.add_suffix(&edits(&tag.path));
    for child in &tag.children {
        row.add_row(&node(child));
    }
    row.upcast()
}

fn leaf_row(title: &str, path: &str, count: i64) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title))
        .subtitle(photos(count))
        .build();
    row.add_suffix(&edits(path));
    row
}

/// Rename or Move, Merge Into and Delete for one tag.
fn edits(path: &str) -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for (label, action) in [
        ("Rename or Move…", "win.tag-rename"),
        ("Merge Into…", "win.tag-merge"),
        ("Delete", "win.tag-delete"),
    ] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&path.to_variant()));
        menu.append_item(&item);
    }
    more(&menu, &format!("Edit {path}"))
}

fn more(menu: &gio::Menu, label: &str) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .valign(gtk::Align::Center)
        .menu_model(menu)
        .tooltip_text("More")
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn photos(count: i64) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}
