//! The questions asked before a photo is written to or put back, the same wherever the write
//! comes from: the Tools tab's preview or one photo edited by hand.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use photomanager_core::history;
use photomanager_core::journal::Pass;
use photomanager_core::settings;

use crate::library::Library;

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
            "about itself is changed, never the picture, every change is written down and the ",
            "last one can be taken back.\n\nEven so: confirm that these photos are backed up ",
            "somewhere else."
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

/// Asks before the last applied pass is taken back.
pub fn before_undo(parent: &impl IsA<gtk::Widget>, pass: &Pass, undo: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::new(
        Some("Take the Last Change Back?"),
        Some(&format!(
            "{} photos were changed on {}. Every one of them gets back what it said before.",
            pass.written, pass.started_at
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("undo", "Take It Back")]);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(None, move |_: &adw::AlertDialog, response: &str| {
        if response == "undo" {
            undo();
        }
    });
    dialog.present(Some(parent));
}

/// Asks before any pass from the history is taken back, and says up front how many of its
/// photos were changed again since and so will be left as they are.
pub fn before_take_back(parent: &impl IsA<gtk::Widget>, pass: &history::Pass, undo: impl Fn() + 'static) {
    let mut body = format!(
        "\u{201c}{}\u{201d} changed {} on {}. Each gets back what it said before.",
        pass.title,
        photos(pass.written),
        pass.started_at
    );
    if pass.changed_since > 0 {
        let (was, left) = match pass.changed_since {
            1 => ("was", "It is left as it is"),
            _ => ("were", "They are left as they are"),
        };
        body.push_str(&format!(
            "\n\n{} of these {} {was} changed again since. {left}.",
            pass.changed_since,
            photos(pass.written)
        ));
    }
    let dialog = adw::AlertDialog::new(Some("Take This Change Back?"), Some(&body));
    dialog.add_responses(&[("cancel", "Cancel"), ("undo", "Take It Back")]);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(None, move |_: &adw::AlertDialog, response: &str| {
        if response == "undo" {
            undo();
        }
    });
    dialog.present(Some(parent));
}

fn photos(count: i64) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
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
