//! The questions asked before a photo is written, the same wherever the write comes from: the
//! Tools tab's preview, the suggestions, or one photo edited by hand. Before the first write of
//! all, whether the photos are backed up; and after a pass, whether to write anyway the photos
//! whose maker note ExifTool doubts.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use photomanager_core::changeset::{Anyway, Asked};
use photomanager_core::settings;

use crate::library::{Library, Reply};

/// Writes at once, or first asks whether the photos are backed up when nothing was ever written.
pub fn before_first_write(parent: &impl IsA<gtk::Widget>, library: &Rc<Library>, write: impl Fn() + 'static) {
    match library.must_ask() {
        true => ask_about_the_backup(parent, library.clone(), write),
        false => write(),
    }
}

/// We cannot know that a backup exists, so this asks rather than pretends to check. What the
/// user names is looked at and described; that is help, never proof.
fn ask_about_the_backup(parent: &impl IsA<gtk::Widget>, library: Rc<Library>, write: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::new(
        Some("Change Photos in the Library?"),
        Some(concat!(
            "This is the first time photoManager writes to your photos. Only what a photo says ",
            "about itself is changed, never the picture, and every write is checked before it ",
            "replaces the file.\n\nA change cannot be taken back in photoManager: your backup ",
            "is the way back. Confirm that these photos are backed up somewhere else."
        )),
    );
    let entry = adw::EntryRow::builder().title("Where the backup is (optional)").build();
    let told = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .label("A location here is only looked at, never taken as proof.")
        .build();
    told.add_css_class("dim-label");
    entry.connect_changed(glib::clone!(
        #[weak]
        told,
        move |entry| told.set_label(&looked_at(&entry.text()))
    ));

    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    box_.append(&group);
    box_.append(&told);
    dialog.set_extra_child(Some(&box_));

    dialog.add_responses(&[("cancel", "Cancel"), ("write", "My Photos Are Backed Up")]);
    dialog.set_response_appearance("write", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(
        None,
        glib::clone!(
            #[weak]
            entry,
            move |_: &adw::AlertDialog, response: &str| {
                if response != "write" {
                    tracing::info!("the first write was declined, nothing was changed");
                    return;
                }
                let named = entry.text().trim().to_string();
                let backup = (!named.is_empty()).then(|| PathBuf::from(named));
                library.acknowledge(backup.as_deref());
                write();
            }
        ),
    );
    dialog.present(Some(parent));
}

/// What can be said about a named backup location, and what cannot.
pub fn looked_at(named: &str) -> String {
    let named = named.trim();
    if named.is_empty() {
        return "A location here is only looked at, never taken as proof.".to_string();
    }
    format!(
        "{} That is what is there, not proof that your photos are in it.",
        settings::look_at(Path::new(named)).tells()
    )
}

/// Explains the photos a pass left because ExifTool doubts their maker note, by camera, and asks
/// whether to write them anyway. Closing the dialog leaves them. When more passes of the same
/// apply follow, the answer can be given for them too, for this apply alone.
pub fn write_anyway(parent: &impl IsA<gtk::Widget>, asked: &Asked, reply: Reply) {
    let photos = asked.photos();
    let counted = match photos {
        1 => "1 photo was".to_string(),
        photos => format!("{photos} photos were"),
    };
    let body = format!(
        "{counted} not written. The camera stored a <i>maker note</i> - its own private part of \
        the photo's data, with things like its settings and a small preview - in a way that does \
        not follow the usual layout.\n\n\
        To add the change, the maker note has to move to another place inside the file. ExifTool \
        cannot be fully sure it moves it correctly, so it stopped and left the photos as they were.\n\n\
        <b>Write Anyway</b> copies the maker note as it is. Afterwards every value in it, the \
        preview, the thumbnail and the picture itself are compared with the original; if anything \
        differs, or ExifTool would leave out a part of the maker note it cannot read, that photo is \
        left exactly as it was. A maker note that was damaged stays as damaged as before - it gets \
        neither better nor worse.\n\n\
        <b>Skip</b> leaves these photos untouched. They will be offered again."
    );
    let dialog = adw::AlertDialog::new(Some("Some Photos Need Your OK"), Some(&body));
    dialog.set_body_use_markup(true);

    let cameras = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    cameras.add_css_class("boxed-list");
    for doubted in &asked.doubted {
        let camera = glib::markup_escape_text(doubted.camera.as_deref().unwrap_or("A camera that does not say"));
        let row = adw::ExpanderRow::builder()
            .title(format!(
                "{camera} \u{b7} {}",
                match doubted.photos.len() {
                    1 => "1 photo".to_string(),
                    photos => format!("{photos} photos"),
                }
            ))
            .subtitle(glib::markup_escape_text(&explained(&doubted.why)))
            .build();
        for rel_path in &doubted.photos {
            let photo = adw::ActionRow::builder()
                .title(glib::markup_escape_text(
                    rel_path.rsplit('/').next().unwrap_or(rel_path),
                ))
                .subtitle(glib::markup_escape_text(rel_path))
                .build();
            row.add_row(&photo);
        }
        cameras.append(&row);
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&cameras);
    let rest = gtk::CheckButton::builder()
        .label("Do the same for the rest of this apply, without asking")
        .visible(asked.more_to_come)
        .build();
    content.append(&rest);
    dialog.set_extra_child(Some(&content));

    dialog.add_responses(&[("skip", "Skip"), ("write", "Write Anyway")]);
    dialog.set_default_response(Some("skip"));
    dialog.set_close_response("skip");
    let reply = std::cell::Cell::new(Some(reply));
    dialog.connect_response(
        None,
        glib::clone!(
            #[weak]
            rest,
            move |_: &adw::AlertDialog, response: &str| {
                let answer = match (response, rest.is_active()) {
                    ("write", true) => Anyway::WriteAll,
                    ("write", false) => Anyway::Write,
                    _ => Anyway::Skip,
                };
                tracing::info!(?answer, "doubted photos answered");
                if let Some(reply) = reply.take() {
                    reply.answer(answer);
                }
            }
        ),
    );
    dialog.present(Some(parent));
}

/// ExifTool's reason in a sentence a person can weigh.
fn explained(why: &str) -> String {
    let lower = why.to_lowercase();
    match () {
        _ if lower.contains("truncated") => {
            "The maker note is cut short: the camera wrote less than it announced".to_string()
        }
        _ if lower.contains("offset") => "The maker note's parts are not where its table says they are".to_string(),
        _ if lower.contains("could not be parsed") => "The maker note is in a shape ExifTool cannot read".to_string(),
        _ => format!("ExifTool says: {why}"),
    }
}
