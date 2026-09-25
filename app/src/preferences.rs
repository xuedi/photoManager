//! Preferences: where Immich is, the key to read it with, and where the library lies inside it.
//! The address and the path are kept in `app.db`; the key goes to the keyring and is never shown
//! again.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use photomanager_core::immich;
use photomanager_core::settings;

use crate::library::Library;
use crate::secrets;

pub fn present(parent: &impl IsA<gtk::Widget>, library: Rc<Library>) {
    let dialog = adw::PreferencesDialog::builder().title("Preferences").build();
    let page = adw::PreferencesPage::builder()
        .title("Immich")
        .icon_name("system-users-symbolic")
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Immich")
        .description(
            "Who is in the photos is read from Immich, and never written back. Keep its face import \
             off: the faces written into the photos would be doubled there.",
        )
        .build();

    let address = adw::EntryRow::builder()
        .title("Server Address")
        .show_apply_button(true)
        .input_purpose(gtk::InputPurpose::Url)
        .text(library.immich_address().unwrap_or_default())
        .build();
    let key = adw::PasswordEntryRow::builder()
        .title("API Key")
        .show_apply_button(true)
        .build();
    let kept = adw::ActionRow::builder()
        .title("Keyring")
        .subtitle("Looking for a kept key")
        .build();
    kept.add_css_class("property");
    let prefix = adw::EntryRow::builder()
        .title("Library Path in Immich")
        .show_apply_button(true)
        .text(library.immich_prefix().unwrap_or_default())
        .build();
    let test = adw::ButtonRow::builder().title("Test Connection").build();
    let told = adw::ActionRow::builder()
        .title("Connection")
        .subtitle("Not tested yet")
        .subtitle_lines(4)
        .build();
    told.add_css_class("property");

    group.add(&address);
    group.add(&key);
    group.add(&kept);
    group.add(&prefix);
    let path_hint = adw::PreferencesGroup::builder()
        .description(
            "Leave the library path empty to take it from Immich's own libraries. The key needs to \
             read assets, persons, faces and libraries, and nothing more.",
        )
        .build();
    let testing = adw::PreferencesGroup::new();
    testing.add(&told);
    testing.add(&test);
    page.add(&group);
    page.add(&path_hint);
    page.add(&testing);
    dialog.add(&page);

    let show_kept = glib::clone!(
        #[weak]
        kept,
        move || {
            glib::spawn_future_local(async move {
                kept.set_subtitle(&match secrets::immich_key().await {
                    Ok(Some(_)) => "An API key is kept in the keyring".to_string(),
                    Ok(None) => "No API key kept yet".to_string(),
                    Err(why) => why,
                });
            });
        }
    );
    show_kept();

    address.connect_apply(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        library,
        move |row| match immich::address(&row.text()) {
            Ok(url) => {
                row.set_text(&url);
                library.put_setting(settings::IMMICH_URL, &url);
                tracing::info!(url, "immich address set");
                dialog.add_toast(adw::Toast::new("The address is kept"));
            }
            Err(why) => dialog.add_toast(adw::Toast::new(&why)),
        }
    ));
    key.connect_apply(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        show_kept,
        move |row| {
            let typed = row.text().to_string();
            row.set_text("");
            if typed.trim().is_empty() {
                return;
            }
            let show_kept = show_kept.clone();
            glib::spawn_future_local(async move {
                match secrets::keep_immich_key(&typed).await {
                    Ok(()) => {
                        tracing::info!("immich key kept in the keyring");
                        dialog.add_toast(adw::Toast::new("The key is kept in the keyring"));
                    }
                    Err(why) => dialog.add_toast(adw::Toast::new(&why)),
                }
                show_kept();
            });
        }
    ));
    prefix.connect_apply(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        library,
        move |row| {
            let typed = row.text().trim().trim_end_matches('/').to_string();
            row.set_text(&typed);
            library.put_setting(settings::IMMICH_PREFIX, &typed);
            dialog.add_toast(adw::Toast::new(match typed.is_empty() {
                true => "The library path is taken from Immich",
                false => "The library path is kept",
            }));
        }
    ));
    test.connect_activated(glib::clone!(
        #[weak]
        told,
        #[strong]
        library,
        move |_| {
            let Some(url) = library.immich_address() else {
                told.set_subtitle("Set the server address first");
                return;
            };
            told.set_subtitle("Asking Immich");
            let library = library.clone();
            glib::spawn_future_local(async move {
                let key = match secrets::immich_key().await {
                    Ok(Some(key)) => key,
                    Ok(None) => {
                        told.set_subtitle("Keep an API key first");
                        return;
                    }
                    Err(why) => {
                        told.set_subtitle(&why);
                        return;
                    }
                };
                library.check_immich(&url, key, move |checked| {
                    let text = match checked {
                        Ok(found) => found,
                        Err(why) => why,
                    };
                    tracing::info!(result = text, "immich connection tested");
                    told.set_subtitle(&glib::markup_escape_text(&text));
                });
            });
        }
    ));

    dialog.present(Some(parent));
}
