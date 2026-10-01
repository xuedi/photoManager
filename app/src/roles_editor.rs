//! The tag roles: which root of the tags holds people, places, the year and the events, how a
//! places tag names its country, and which generated roles stay tags. Proposed from the roots the
//! tags have until saved.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use photomanager_core::roles::{CountryLevel, Role, Roles};

use crate::library::Library;

const DESCRIBED: &str = "Which root of the tags says what a field of its own says too. The suggestions that move a \
                         tag into its field read it, and Redundant Tags takes such a tag away only where its \
                         field is proved to say it.";

const NONE: &str = "None";

/// The group, filled with the roles kept, or the ones proposed while none are.
pub fn group(library: Rc<Library>, told: impl Fn(&str) + 'static) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Tag Roles")
        .description(DESCRIBED)
        .build();
    let kept = library.kept_roles();
    let roles = kept.clone().unwrap_or_else(|| library.proposed_roles());
    let mut roots: Vec<String> = library.roots_counted().into_iter().map(|(root, _)| root).collect();
    for root in roles.roots.values() {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
    let names: Vec<&str> = std::iter::once(NONE).chain(roots.iter().map(String::as_str)).collect();

    let combos: Vec<(Role, adw::ComboRow)> = Role::ALL
        .into_iter()
        .map(|role| {
            let combo = adw::ComboRow::builder()
                .title(role.title())
                .subtitle(format!("Tags of {}", role.says()))
                .model(&gtk::StringList::new(&names))
                .build();
            let at = roles
                .root(role)
                .and_then(|root| roots.iter().position(|each| each == root))
                .map_or(0, |at| at + 1);
            combo.set_selected(at as u32);
            group.add(&combo);
            (role, combo)
        })
        .collect();

    let levels: Vec<&str> = CountryLevel::ALL.iter().map(|level| level.tells()).collect();
    let country = adw::ComboRow::builder()
        .title("Country in a Places Tag")
        .model(&gtk::StringList::new(&levels))
        .build();
    let at = CountryLevel::ALL
        .iter()
        .position(|level| *level == roles.country)
        .unwrap_or(0);
    country.set_selected(at as u32);
    group.add(&country);

    let switches: Vec<(Role, adw::SwitchRow)> = Role::ALL
        .into_iter()
        .filter(|role| role.generated())
        .map(|role| {
            let switch = adw::SwitchRow::builder()
                .title(format!(
                    "Keep {} Tags",
                    match role {
                        Role::Events => "Event",
                        other => other.title(),
                    }
                ))
                .subtitle("Made from the data by Tidy Tags, never taken away as redundant")
                .active(roles.keeps(role))
                .build();
            group.add(&switch);
            (role, switch)
        })
        .collect();

    let state = adw::ActionRow::builder()
        .title("Kept")
        .subtitle(state_of(kept.is_some()))
        .build();
    state.add_css_class("property");
    group.add(&state);
    let save = adw::ButtonRow::builder().title("Save Roles").build();
    save.add_css_class("suggested-action");
    group.add(&save);
    let proposal = adw::ButtonRow::builder()
        .title("Use the Proposal")
        .visible(kept.is_some())
        .build();
    group.add(&proposal);

    let told = Rc::new(told);
    save.connect_activated(glib::clone!(
        #[strong]
        library,
        #[strong]
        told,
        #[weak]
        state,
        #[weak]
        proposal,
        #[weak]
        country,
        move |_| {
            let chosen = Roles {
                roots: combos
                    .iter()
                    .filter_map(|(role, combo)| {
                        let at = combo.selected() as usize;
                        (at > 0)
                            .then(|| (*role, roots.get(at - 1).cloned()))
                            .and_then(|(role, root)| Some((role, root?)))
                    })
                    .collect(),
                country: CountryLevel::ALL[(country.selected() as usize).min(CountryLevel::ALL.len() - 1)],
                kept: switches
                    .iter()
                    .filter(|(_, switch)| switch.is_active())
                    .map(|(role, _)| *role)
                    .collect(),
            };
            match library.set_roles(Some(&chosen)) {
                Ok(()) => {
                    state.set_subtitle(state_of(true));
                    proposal.set_visible(true);
                    told("Tag roles saved");
                }
                Err(why) => told(&format!("The roles were not saved: {why}")),
            }
        }
    ));
    proposal.connect_activated(glib::clone!(
        #[strong]
        library,
        #[strong]
        told,
        #[weak]
        state,
        move |proposal| match library.set_roles(None) {
            Ok(()) => {
                state.set_subtitle(state_of(false));
                proposal.set_visible(false);
                told("The proposal from the tags is used, reopen Preferences to see it");
            }
            Err(why) => told(&format!("The roles were not changed: {why}")),
        }
    ));
    group
}

fn state_of(kept: bool) -> &'static str {
    match kept {
        true => "Saved",
        false => "Proposed from the roots the tags have, not saved yet",
    }
}
