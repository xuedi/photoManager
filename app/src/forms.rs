//! The form each edit asks its value in: a place from the search or a pin on the map, a shift per
//! camera of the scope, a date, a zone, a tag from the tree or a new one, a folder by its parts.
//! Preview closes the form and shows the change set; nothing is written from here.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use photomanager_core::dates::Shift;
use photomanager_core::edits::{Edit, Value};
use photomanager_core::layout::Component;
use photomanager_core::scope::Scope;
use photomanager_core::tools::folders::Parts;
use photomanager_core::tools::tag_vocabulary::Generated;
use photomanager_core::tools::{Answer, Located};

use crate::library::Library;
use crate::panel::map_at;
use crate::tools::Tools;

type Make = Rc<dyn Fn() -> Result<Value, String>>;

/// Shows the form of an edit for the scope.
pub fn present(tools: &Tools, library: &Rc<Library>, edit: Edit, scope: &Scope) {
    match edit {
        Edit::SetPlace => place(tools, library, edit),
        Edit::ShiftDates => shift(tools, library, edit, scope),
        Edit::SetDate => {
            let entry = adw::EntryRow::builder().title("Date").build();
            let group = described("YYYY-MM-DD HH:MM:SS. Each photo after the first by name is given a second more.");
            group.add(&entry);
            let make: Make = Rc::new(glib::clone!(
                #[weak]
                entry,
                #[upgrade_or]
                Err("the form is closed".to_string()),
                move || edit.read(entry.text().as_str())
            ));
            dialog(tools, edit, &[group.upcast_ref()], make, Some(entry.upcast_ref()));
        }
        Edit::SetTimeZone => {
            let taken = adw::SwitchRow::builder()
                .title("Where Each Photo Was Taken")
                .subtitle("A photo that states an offset keeps it")
                .active(true)
                .build();
            let zone = adw::EntryRow::builder().title("Time Zone").sensitive(false).build();
            taken
                .bind_property("active", &zone, "sensitive")
                .invert_boolean()
                .sync_create()
                .build();
            let group = described("An IANA zone such as Europe/Berlin writes its offset on every dated photo.");
            group.add(&taken);
            group.add(&zone);
            let make: Make = Rc::new(glib::clone!(
                #[weak]
                taken,
                #[weak]
                zone,
                #[upgrade_or]
                Err("the form is closed".to_string()),
                move || match taken.is_active() {
                    true => edit.read(""),
                    false if zone.text().trim().is_empty() => Err("type a time zone".to_string()),
                    false => edit.read(zone.text().as_str()),
                }
            ));
            dialog(tools, edit, &[group.upcast_ref()], make, None);
        }
        Edit::AddTag | Edit::RemoveTag | Edit::RenameTag => tag(tools, library, edit),
        Edit::TidyTags => {
            let names: Vec<&str> = Generated::ALL.iter().map(|generated| generated.tells()).collect();
            let generated = adw::ComboRow::builder()
                .title("Generated Tags")
                .subtitle("The year, the place and the event")
                .model(&gtk::StringList::new(&names))
                .build();
            let group = described(
                "Every tag field is written the same. The generated tags are made from the date, the place words and the folder, dropped, or left as they are.",
            );
            group.add(&generated);
            let make: Make = Rc::new(glib::clone!(
                #[weak]
                generated,
                #[upgrade_or]
                Err("the form is closed".to_string()),
                move || {
                    let chosen = Generated::ALL[generated.selected().min(2) as usize];
                    Ok(Value::Generated(chosen))
                }
            ));
            dialog(tools, edit, &[group.upcast_ref()], make, None);
        }
        Edit::MoveEvent => folder(tools, library, edit, scope),
        Edit::PositionFromNeighbour => tools.open_neighbour_of_scope(library),
        #[cfg(feature = "devtools")]
        Edit::Rating => {
            let entry = adw::EntryRow::builder().title("Rating").text("3").build();
            let group = described("From 0 to 5.");
            group.add(&entry);
            let make: Make = Rc::new(glib::clone!(
                #[weak]
                entry,
                #[upgrade_or]
                Err("the form is closed".to_string()),
                move || edit.read(entry.text().as_str())
            ));
            dialog(tools, edit, &[group.upcast_ref()], make, Some(entry.upcast_ref()));
        }
    }
}

fn described(text: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder().description(text).build()
}

/// The dialog every form is: its parts, what is wrong with the value, and Preview.
fn dialog(tools: &Tools, edit: Edit, parts: &[&gtk::Widget], make: Make, focus: Option<&gtk::Widget>) -> adw::Dialog {
    let why = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    why.add_css_class("error");
    let button = gtk::Button::builder().label("Preview").build();
    button.add_css_class("suggested-action");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    for part in parts {
        content.append(*part);
    }
    content.append(&why);
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&button);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scrolled));
    let dialog = adw::Dialog::builder()
        .title(edit.title())
        .content_width(480)
        .child(&view)
        .build();
    let preview = glib::clone!(
        #[weak]
        tools,
        #[weak]
        why,
        #[weak]
        dialog,
        move || match make() {
            Ok(value) => {
                dialog.close();
                tools.preview_edit(edit, value);
            }
            Err(reason) => {
                why.set_label(&reason);
                why.set_visible(true);
            }
        }
    );
    let preview = Rc::new(preview);
    button.connect_clicked(glib::clone!(
        #[strong]
        preview,
        move |_| preview()
    ));
    if let Some(entry) = focus.and_then(|focus| focus.downcast_ref::<adw::EntryRow>()) {
        entry.connect_entry_activated(move |_| preview());
    }
    dialog.present(Some(tools));
    if let Some(focus) = focus {
        focus.grab_focus();
    }
    tracing::info!(edit = edit.key(), "edit form shown");
    dialog
}

/// A place by its name, each found one a row that previews it, or Pick on Map.
fn place(tools: &Tools, library: &Rc<Library>, edit: Edit) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .build();
    list.add_css_class("boxed-list");
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search Places")
        .hexpand(true)
        .build();
    search.update_property(&[gtk::accessible::Property::Label("Search Places")]);
    let map = gtk::Button::builder()
        .label("Pick on Map")
        .halign(gtk::Align::Center)
        .build();
    map.add_css_class("pill");
    let chosen: Rc<RefCell<Option<Answer>>> = Rc::default();
    let make: Make = Rc::new(glib::clone!(
        #[strong]
        chosen,
        move || match chosen.borrow().clone() {
            Some(answer) => Ok(Value::Place(answer)),
            None => Err("choose a place, or pick a point on the map".to_string()),
        }
    ));
    let dialog = dialog(
        tools,
        edit,
        &[search.upcast_ref(), list.upcast_ref(), map.upcast_ref()],
        make,
        Some(search.upcast_ref()),
    );
    dialog.set_content_height(560);

    let fill = glib::clone!(
        #[weak]
        list,
        #[weak]
        tools,
        #[weak]
        dialog,
        #[strong]
        library,
        move |text: &str| {
            list.remove_all();
            if text.trim().is_empty() {
                return;
            }
            let found = library.find_place(text);
            if found.is_empty() {
                list.append(
                    &adw::ActionRow::builder()
                        .title(match library.counts().places {
                            0 => "There is no place data yet: get it on the dashboard",
                            _ => "Nothing by that name in the place data",
                        })
                        .build(),
                );
            }
            for candidate in found {
                let place = Located::of(&candidate.place);
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&place.tells()))
                    .activatable(true)
                    .build();
                let answer = Answer::Place(place);
                row.connect_activated(glib::clone!(
                    #[weak]
                    tools,
                    #[weak]
                    dialog,
                    move |_| {
                        dialog.close();
                        tools.preview_edit(edit, Value::Place(answer.clone()));
                    }
                ));
                list.append(&row);
            }
        }
    );
    search.connect_search_changed(move |search| fill(search.text().as_str()));
    map.connect_clicked(glib::clone!(
        #[weak]
        tools,
        #[weak]
        dialog,
        #[strong]
        library,
        move |_| {
            dialog.close();
            pick_on_map(&tools, &library, edit);
        }
    ));
}

/// A map to drop a pin on: the place the pin is in named under it, and Preview. The tiles come
/// from OpenStreetMap, and only while it is open.
fn pick_on_map(tools: &Tools, library: &Rc<Library>, edit: Edit) {
    let (map, mark) = map_at(20.0, 0.0);
    map.set_height_request(360);
    map.set_vexpand(true);
    if let Some(viewport) = map.viewport() {
        viewport.set_zoom_level(2.0);
    }
    mark.set_visible(false);
    let near = gtk::Label::builder()
        .label("Click the map where the photos were taken")
        .xalign(0.0)
        .wrap(true)
        .build();
    near.update_property(&[gtk::accessible::Property::Label("Chosen Point")]);
    let pin: Rc<RefCell<Option<Answer>>> = Rc::default();
    let click = gtk::GestureClick::new();
    click.connect_released(glib::clone!(
        #[weak]
        near,
        #[strong]
        pin,
        #[strong]
        library,
        move |gesture, _, x, y| {
            let Some(map) = gesture.widget().and_downcast::<shumate::SimpleMap>() else {
                return;
            };
            let Some(viewport) = map.viewport() else {
                return;
            };
            let (lat, lon) = viewport.widget_coords_to_location(&map, x, y);
            shumate::prelude::LocationExt::set_location(&mark, lat, lon);
            mark.set_visible(true);
            let found = library.nearest(lat, lon).and_then(|at| Answer::pin(lat, lon, &at));
            near.set_label(&match &found {
                Some(pin) => pin.tells(),
                None if library.counts().places == 0 => {
                    "There is no place data yet: get it on the dashboard".to_string()
                }
                None => "No place near this point".to_string(),
            });
            *pin.borrow_mut() = found;
        }
    ));
    map.add_controller(click);
    let make: Make = Rc::new(move || match pin.borrow().clone() {
        Some(pin) => Ok(Value::Place(pin)),
        None => Err("click the map where the photos were taken".to_string()),
    });
    let dialog = dialog(tools, edit, &[map.upcast_ref(), near.upcast_ref()], make, None);
    dialog.set_content_width(640);
    dialog.set_content_height(560);
    tracing::info!("map shown to pick a point");
}

/// One entry per camera of the scope, each empty for a camera that was right.
fn shift(tools: &Tools, library: &Rc<Library>, edit: Edit, scope: &Scope) {
    let cameras = match library.cameras(scope) {
        Ok(cameras) => cameras,
        Err(why) => {
            tracing::error!(why, "the cameras could not be read");
            return;
        }
    };
    let hint = gtk::Label::builder()
        .label("How far each camera's clock was off, such as -640d or +1y 2d 03:00. Leave a camera empty when it was right.")
        .xalign(0.0)
        .wrap(true)
        .build();
    hint.add_css_class("dim-label");
    let mut parts: Vec<gtk::Widget> = vec![hint.upcast()];
    let mut entries = Vec::new();
    if cameras.is_empty() {
        parts.push(described("No photo of the scope has a date.").upcast());
    }
    for camera in &cameras {
        let group = adw::PreferencesGroup::builder()
            .title(glib::markup_escape_text(&camera.name))
            .description(glib::markup_escape_text(&camera.facts()))
            .build();
        let entry = adw::EntryRow::builder()
            .title(format!("Shift for {}", camera.name))
            .build();
        group.add(&entry);
        parts.push(group.upcast());
        entries.push((camera.name.clone(), entry));
    }
    let focus = entries.first().map(|(_, entry)| entry.clone().upcast::<gtk::Widget>());
    let make: Make = Rc::new(move || {
        let mut shifts = Vec::new();
        for (camera, entry) in &entries {
            let text = entry.text();
            if text.trim().is_empty() {
                continue;
            }
            shifts.push((
                camera.clone(),
                Shift::read(&text).map_err(|why| format!("{camera}: {why}"))?,
            ));
        }
        match shifts.is_empty() {
            true => Err("type a shift for at least one camera".to_string()),
            false => Ok(Value::Shift(shifts)),
        }
    });
    let children: Vec<&gtk::Widget> = parts.iter().collect();
    dialog(tools, edit, &children, make, focus.as_ref());
}

/// A tag typed, or chosen from the tree with the search; Rename Tag asks its new name too.
fn tag(tools: &Tools, library: &Rc<Library>, edit: Edit) {
    let entry = adw::EntryRow::builder().title("Tag").build();
    let renamed = adw::EntryRow::builder().title("New Name").build();
    let group = described(match edit {
        Edit::AddTag => {
            "Its levels separated by /, such as people/family/Anna. Choose one of the tree below or type a new one."
        }
        Edit::RemoveTag => "It and every tag below it are taken off the photos of the scope.",
        _ => "It and every tag below it move to the new name, which merges them into a tag that is there already.",
    });
    group.add(&entry);
    if edit == Edit::RenameTag {
        group.add(&renamed);
    }
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .build();
    list.add_css_class("boxed-list");
    let make: Make = Rc::new(glib::clone!(
        #[weak]
        entry,
        #[weak]
        renamed,
        #[upgrade_or]
        Err("the form is closed".to_string()),
        move || match edit {
            Edit::RenameTag => edit.read(&format!("{} -> {}", entry.text(), renamed.text())),
            _ => edit.read(entry.text().as_str()),
        }
    ));
    let dialog = dialog(
        tools,
        edit,
        &[group.upcast_ref(), list.upcast_ref()],
        make,
        Some(entry.upcast_ref()),
    );
    dialog.set_content_height(560);

    let tags: Rc<RefCell<Vec<(String, i64)>>> = Rc::default();
    let fill = glib::clone!(
        #[weak]
        list,
        #[weak]
        entry,
        #[weak]
        renamed,
        #[strong]
        tags,
        move |text: &str| {
            list.remove_all();
            let wanted = text.trim().to_lowercase();
            for (path, count) in tags
                .borrow()
                .iter()
                .filter(|(path, _)| path.to_lowercase().contains(&wanted))
                .take(50)
            {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(path))
                    .subtitle(match count {
                        1 => "1 photo".to_string(),
                        count => format!("{count} photos"),
                    })
                    .activatable(true)
                    .build();
                let path = path.clone();
                row.connect_activated(glib::clone!(
                    #[weak]
                    entry,
                    #[weak]
                    renamed,
                    move |_| {
                        entry.set_text(&path);
                        renamed.set_text(&path);
                        renamed.grab_focus();
                    }
                ));
                list.append(&row);
            }
        }
    );
    let fill = Rc::new(fill);
    entry.connect_changed(glib::clone!(
        #[strong]
        fill,
        move |entry| fill(entry.text().as_str())
    ));
    library.sidebars(glib::clone!(
        #[weak]
        entry,
        #[strong]
        tags,
        move |found| match found {
            Ok(sidebars) => {
                *tags.borrow_mut() = sidebars
                    .tags
                    .nodes()
                    .map(|(path, count)| (path.to_string(), count))
                    .collect();
                fill(entry.text().as_str());
            }
            Err(why) => tracing::error!(why, "the tags could not be read"),
        }
    ));
}

/// The parts of the folder the scope's event goes into, with where it is now and where it would
/// be, said again with every letter typed.
fn folder(tools: &Tools, library: &Rc<Library>, edit: Edit, scope: &Scope) {
    let question = match library.move_proposal(scope) {
        Ok(question) => question,
        Err(why) => {
            tools.say(&format!("Move Event: {why}"));
            return;
        }
    };
    let layout = library.layout();
    let parts = Parts::of(&question, &layout);
    let group = adw::PreferencesGroup::builder()
        .title(layout.title())
        .description("The date stays as the folder says it.")
        .build();
    let entry = |title: &str, text: &str| {
        let entry = adw::EntryRow::builder().title(title).text(text).build();
        group.add(&entry);
        entry
    };
    let levels: Vec<(Component, adw::EntryRow)> = parts
        .named
        .iter()
        .map(|(component, text)| (component.clone(), entry(&component.title(), text)))
        .collect();
    let date = entry("Date", &parts.date);
    date.set_editable(false);
    date.set_sensitive(false);
    let name = entry("Event", &parts.name);

    let paths = adw::PreferencesGroup::new();
    let now = adw::ActionRow::builder()
        .title("Now")
        .subtitle(glib::markup_escape_text(&question.key))
        .subtitle_lines(3)
        .subtitle_selectable(true)
        .build();
    let after = adw::ActionRow::builder().title("After").subtitle_lines(3).build();
    paths.add(&now);
    paths.add(&after);

    let typed = {
        let levels: Vec<(Component, glib::WeakRef<adw::EntryRow>)> = levels
            .iter()
            .map(|(component, entry)| (component.clone(), entry.downgrade()))
            .collect();
        let (date, name) = (date.downgrade(), name.downgrade());
        let text = |entry: &glib::WeakRef<adw::EntryRow>| {
            entry
                .upgrade()
                .map(|entry| entry.text().to_string())
                .unwrap_or_default()
        };
        Rc::new(move || Parts {
            named: levels
                .iter()
                .map(|(component, entry)| (component.clone(), text(entry)))
                .collect(),
            date: text(&date),
            name: text(&name),
            date_fixed: true,
        })
    };
    let said = {
        let (layout, question, typed) = (layout.clone(), question.clone(), typed.clone());
        glib::clone!(
            #[weak]
            after,
            move || after.set_subtitle(&glib::markup_escape_text(&match typed().after(&question, &layout) {
                Ok(path) => path,
                Err(reason) => reason,
            }))
        )
    };
    said();
    let said = Rc::new(said);
    let mut entries: Vec<adw::EntryRow> = levels.iter().map(|(_, entry)| entry.clone()).collect();
    entries.push(name.clone());
    for entry in &entries {
        entry.connect_changed(glib::clone!(
            #[strong]
            said,
            move |_| said()
        ));
    }
    let make: Make = Rc::new(move || Ok(Value::Folder(typed().after(&question, &layout)?)));
    let focus = entries.first().cloned().map(|entry| entry.upcast::<gtk::Widget>());
    dialog(
        tools,
        edit,
        &[group.upcast_ref(), paths.upcast_ref()],
        make,
        focus.as_ref(),
    );
}
