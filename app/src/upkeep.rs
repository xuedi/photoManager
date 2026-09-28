//! The bar of upkeep jobs on the dashboard: a segment per job with its state at a glance, and a
//! popover with what the last run did and the buttons that run it.

use std::cell::RefCell;

use adw::prelude::*;
use photomanager_core::upkeep::{Job, State, Status};

/// The action that runs a job, and what its button says.
pub fn action(job: Job) -> (&'static str, &'static str) {
    match job {
        Job::Scan => ("win.scan", "Scan the Library"),
        Job::Thumbnails => ("win.fill-thumbnails", "Make Missing Thumbnails"),
        Job::Places => ("win.get-places", "Get Place Data"),
        Job::People => ("win.get-people", "Get People from Immich"),
    }
}

fn tells(job: Job) -> &'static str {
    match job {
        Job::Scan => "Reads what the photos say into the cache. Only new and changed photos are read.",
        Job::Thumbnails => "Makes the small pictures the scan could not make.",
        Job::Places => "Downloads the GeoNames dumps and imports them. Nothing else here downloads.",
        Job::People => "Reads who is in the photos from Immich. Immich is only read.",
    }
}

/// What a segment shows on top of the job's status: this session's own knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Now<'a> {
    Idle,
    /// Another job runs; this one waits.
    Waiting,
    Running,
    Failed(&'a str),
}

#[derive(Debug)]
pub struct Segment {
    pub job: Job,
    button: gtk::MenuButton,
    icon: gtk::Image,
    spinner: adw::Spinner,
    caption: gtk::Label,
    popover: gtk::Popover,
    content: gtk::Box,
    /// The state in a word and the caption, as last shown.
    shown: RefCell<(String, String)>,
}

impl Segment {
    pub fn new(job: Job) -> Segment {
        let icon = gtk::Image::from_icon_name("content-loading-symbolic");
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        let spinner = adw::Spinner::builder().visible(false).build();
        let title = gtk::Label::builder()
            .label(job.title())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("heading");
        let caption = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        words.append(&title);
        words.append(&caption);
        let row = gtk::Box::builder()
            .spacing(10)
            .margin_top(4)
            .margin_bottom(4)
            .margin_start(2)
            .margin_end(2)
            .build();
        row.append(&icon);
        row.append(&spinner);
        row.append(&words);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .width_request(280)
            .build();
        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .child(&row)
            .popover(&popover)
            .tooltip_text(tells(job))
            .build();
        Segment {
            job,
            button,
            icon,
            spinner,
            caption,
            popover,
            content,
            shown: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::MenuButton {
        &self.button
    }

    /// The state in a word and the caption, as shown.
    pub fn shown(&self) -> (String, String) {
        self.shown.borrow().clone()
    }

    pub fn show(&self, status: &Status, now: Now) {
        let (icon, style, word) = match (now, status.state) {
            (Now::Running, _) => ("", "", "running"),
            (Now::Failed(_), _) => ("dialog-error-symbolic", "error", "failed"),
            (_, State::Worth) => ("dialog-warning-symbolic", "warning", "worth running"),
            (_, State::Fine) => ("object-select-symbolic", "success", "fine"),
            (_, State::Never) => ("content-loading-symbolic", "dim-label", "never run"),
        };
        let running = now == Now::Running;
        self.spinner.set_visible(running);
        self.icon.set_visible(!running);
        if !running {
            self.icon.set_icon_name(Some(icon));
            self.icon.set_css_classes(&[style]);
        }
        let caption = match now {
            Now::Failed(_) => "did not work".to_string(),
            _ => status.caption.clone(),
        };
        self.caption.set_label(&caption);
        self.button.set_sensitive(matches!(now, Now::Idle | Now::Failed(_)));
        match self.job == Job::Scan && status.state == State::Never && !running {
            true => self.button.add_css_class("suggested-action"),
            false => self.button.remove_css_class("suggested-action"),
        }
        let named = match caption == word {
            true => format!("{}, {word}", self.job.title()),
            false => format!("{}, {word}, {caption}", self.job.title()),
        };
        self.button.update_property(&[gtk::accessible::Property::Label(&named)]);
        *self.shown.borrow_mut() = (word.to_string(), caption);
        self.fill(status, now);
    }

    fn fill(&self, status: &Status, now: Now) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let heading = gtk::Label::builder().label(self.job.title()).xalign(0.0).build();
        heading.add_css_class("heading");
        self.content.append(&heading);
        let about = wrapped(tells(self.job));
        about.add_css_class("dim-label");
        self.content.append(&about);

        if let Now::Failed(why) = now {
            let failed = wrapped(&format!("The last run did not work: {why}"));
            failed.add_css_class("error");
            self.content.append(&failed);
        } else if let Some(reason) = &status.reason {
            let worth = wrapped(reason);
            worth.add_css_class("warning");
            self.content.append(&worth);
        }

        if !status.facts.is_empty() {
            let grid = gtk::Grid::builder().column_spacing(12).row_spacing(4).build();
            for (at, (label, value)) in status.facts.iter().enumerate() {
                let label = gtk::Label::builder().label(*label).xalign(0.0).yalign(0.0).build();
                label.add_css_class("dim-label");
                let value = wrapped(value);
                value.set_hexpand(true);
                grid.attach(&label, 0, at as i32, 1, 1);
                grid.attach(&value, 1, at as i32, 1, 1);
            }
            self.content.append(&grid);
        }

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let (action, label) = action(self.job);
        let run = self.run_button(action, label);
        if matches!(status.state, State::Worth | State::Never) || matches!(now, Now::Failed(_)) {
            run.add_css_class("suggested-action");
        }
        if self.job == Job::Thumbnails {
            run.set_sensitive(status.state == State::Worth);
        }
        buttons.append(&run);
        if self.job == Job::Scan && status.state != State::Never {
            buttons.append(&self.run_button("win.rescan", "Read Every Photo Again"));
        }
        self.content.append(&buttons);
    }

    fn run_button(&self, action: &str, label: &str) -> gtk::Button {
        let button = gtk::Button::builder().label(label).action_name(action).build();
        let popover = self.popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        });
        button
    }
}

fn wrapped(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(36)
        .build()
}
