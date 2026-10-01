//! Actions that let a test drive the window from outside the process. Compiled only with the
//! `devtools` feature.

use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::paths::Paths;
use photomanager_core::scope::Scope;
use std::path::Path;
use std::rc::Rc;

use crate::library::Library;
use crate::window::Window;

pub fn install(app: &adw::Application, window: &Window, paths: &Paths, library: Option<Rc<Library>>) {
    let dump_state = gio::ActionEntry::builder("dump-state")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(glib::clone!(
            #[weak]
            window,
            #[strong]
            paths,
            move |_: &adw::Application, _, parameter| {
                let target = parameter.and_then(|value| value.str()).unwrap_or_default();
                write_out(target, &state(&window, &paths, library.as_deref()));
            }
        ))
        .build();

    let snapshot = gio::ActionEntry::builder("snapshot")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(glib::clone!(
            #[weak]
            window,
            move |_: &adw::Application, _, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                if let Err(error) = snapshot_to_png(&window, Path::new(target)) {
                    tracing::error!(%error, "snapshot failed");
                }
            }
        ))
        .build();

    app.add_action_entries([dump_state, snapshot]);

    // A combo row has nothing a test from outside can click; this picks a rating in the form.
    let rating = gio::ActionEntry::builder("photo-form-rating")
        .parameter_type(Some(glib::VariantTy::INT32))
        .activate(|window: &Window, _, parameter| {
            let stars = parameter.and_then(|value| value.get::<i32>()).unwrap_or(-1);
            window
                .gallery()
                .photo()
                .form_rating((stars >= 0).then_some(i64::from(stars)));
        })
        .build();
    // A headless session has no keyring; this gives the key for this run only.
    let immich_key = gio::ActionEntry::builder("immich-key")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(|_: &Window, _, parameter| {
            if let Some(key) = parameter.and_then(|value| value.str()) {
                crate::secrets::use_for_this_run(key);
            }
        })
        .build();
    // A pointer that rests on a row is nothing a test from outside can make; this frames a face
    // the way pointing at a person in the panel does. An empty name frames none.
    let point_at = gio::ActionEntry::builder("photo-point-at")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(|window: &Window, _, parameter| {
            let name = parameter.and_then(|value| value.str()).filter(|name| !name.is_empty());
            window.gallery().photo().point_at(name);
        })
        .build();
    // Photos ExifTool doubts need a camera that wrote a maker note it doubts; this shows the
    // question about them with invented ones, and drops the answer.
    let doubted = gio::ActionEntry::builder("show-doubted")
        .activate(|window: &Window, _, _| {
            use photomanager_core::changeset::{Asked, Doubted};
            let asked = Asked {
                doubted: vec![
                    Doubted {
                        camera: Some("OLYMPUS X1".to_string()),
                        why: "Truncated MakerNotes directory".to_string(),
                        photos: vec![
                            "Atlantis/2024-05-01 Picnic/P1000001.JPG".to_string(),
                            "Atlantis/2024-05-01 Picnic/P1000002.JPG".to_string(),
                        ],
                    },
                    Doubted {
                        camera: Some("EXAMPLE Z1".to_string()),
                        why: "MakerNotes offsets may be incorrect (fix or ignore?)".to_string(),
                        photos: vec!["Atlantis/2024-06-02 Harbour/DSC_0001.JPG".to_string()],
                    },
                ],
                more_to_come: true,
            };
            crate::confirm::write_anyway(window, &asked, crate::library::Reply::dropped());
        })
        .build();
    window.add_action_entries([rating, immich_key, point_at, doubted]);
}

fn state(window: &Window, paths: &Paths, library: Option<&Library>) -> String {
    let counts = library.map(|library| library.counts()).unwrap_or_default();
    let preview = window.preview();
    let previewed = preview.counts().map(|counts| {
        serde_json::json!({
            "title": preview.title(),
            "photos": counts.photos,
            "change": counts.change,
            "nothing": counts.nothing,
            "refused": counts.refused,
            "written": counts.written,
            "failed": counts.failed,
            "selected": counts.selected,
            "traffic": counts.traffic,
            "asked": preview.asked(),
        })
    });
    let applied = preview.applied().map(|summary| {
        serde_json::json!({
            "written": summary.written,
            "skipped": summary.skipped,
            "refused": summary.refused,
            "failed": summary.failed,
            "cancelled": summary.cancelled,
        })
    });
    let survey = library.and_then(|library| library.survey()).map(|survey| {
        let coverage: serde_json::Map<String, serde_json::Value> = survey
            .coverage
            .iter()
            .map(|(gap, measure)| {
                (
                    gap.key().to_string(),
                    serde_json::json!({ "of": measure.of, "missing": measure.missing }),
                )
            })
            .collect();
        serde_json::json!({
            "photos": survey.photos,
            "events": survey.events,
            "bytes": survey.bytes,
            "first": survey.first,
            "last": survey.last,
            "cameras": survey.camera_count(),
            "coverage": coverage,
            "countries": survey.countries.iter().map(|place| place.name.clone()).collect::<Vec<_>>(),
            "aligned": {
                "events": survey.aligned.events,
                "off_events": survey.aligned.off_events,
                "photos": survey.aligned.photos,
                "reasons": survey.aligned.reasons.iter().map(|finding| serde_json::json!({
                    "title": finding.title,
                    "count": finding.count,
                    "filter": finding.filter.to_string(),
                })).collect::<Vec<_>>(),
            },
            "tidy": survey.tidy.iter().map(|finding| serde_json::json!({
                "title": finding.title,
                "count": finding.count,
                "filter": finding.filter.to_string(),
            })).collect::<Vec<_>>(),
        })
    });
    let dashboard = window.dashboard();
    let shown = window.gallery();
    let gallery = window.shown().map(|(filter, count)| {
        serde_json::json!({
            "filter": filter.to_string(),
            "title": filter.title(),
            "count": count,
            "sort": shown.order().key(),
            "page": shown.page(),
            "loading": shown.is_loading(),
            "listed": shown.listed().len(),
            "pictures": shown.pictures(),
            "kept": shown.kept(),
            "selected": shown.selected(),
            "chips": shown.chips(),
            "dots": shown.dots(),
            "people": shown.people_listed(),
            "toast": shown.toast(),
        })
    });
    let page = shown.photo();
    let photo = shown.photo_open().then(|| {
        serde_json::json!({
            "path": page.path(),
            "position": page.position(),
            "count": page.count(),
            "full": page.full_size().map(|(width, height)| serde_json::json!({ "width": width, "height": height })),
            "failed": page.full_failed(),
            "held": page.held(),
            "thumb_ms": page.timings().0,
            "full_ms": page.timings().1,
            "picture": page.shows_picture(),
            "panel": page.shows_panel(),
            "map": page.shows_map(),
            "toast": page.toast(),
            "editing": page.editing(),
            "pending": page.pending().map(|pending| match pending {
                Ok(fields) => serde_json::json!(fields),
                Err(why) => serde_json::json!({ "refused": why }),
            }),
            "review": page.review_lines(),
            "applied": page.applied().map(|summary| serde_json::json!({
                "written": summary.written,
            })),
            "details": page.details().map(|details| serde_json::json!({
                "taken_at": details.taken_at,
                "offset": details.taken_offset,
                "gps": details.gps.map(|(lat, lon)| [lat, lon]),
                "derived": details.derived_from(),
                "tags": details.tags,
                "rating": details.rating,
                "content_id": details.content_id,
            })),
        })
    });
    let scope = match window.scope() {
        Scope::Filter(filter) => serde_json::json!({ "filter": filter.to_string(), "title": filter.title() }),
        Scope::Photos { title, paths } => serde_json::json!({ "title": title, "paths": paths }),
    };
    let tools = serde_json::json!({
        "photos": window.tools().scope_photos(),
        "toast": window.tools().toast(),
    });
    let listed = window.suggestions();
    let ticked = listed.ticked();
    let suggestions = serde_json::json!({
        "busy": listed.is_busy(),
        "fixes": listed.found().iter().map(|fix| serde_json::json!({
            "key": fix.key,
            "finder": fix.finder,
            "title": fix.title,
            "detail": fix.detail,
            "photos": fix.photos,
            "lines": fix.lines.len(),
            "ticked": ticked.contains(&fix.key),
        })).collect::<Vec<_>>(),
        "aside": listed.aside().iter().map(|fix| &fix.key).collect::<Vec<_>>(),
        "applied": listed.applied().map(|passes| passes.iter().map(|pass| serde_json::json!({
            "finder": pass.finder,
            "written": pass.summary.written,
            "refused": pass.summary.refused,
        })).collect::<Vec<_>>()),
        "dashboard": dashboard.suggestions_line(),
        "toast": listed.toast(),
    });
    serde_json::json!({
        "survey": survey,
        "field": dashboard.field().key(),
        "listed": dashboard.listed_places(),
        "gallery": gallery,
        "photo": photo,
        "scope": scope,
        "tools": tools,
        "suggestions": suggestions,
        "page": window.tools().showing(),
        "preview": previewed,
        "applied": applied,
        "toast": preview.toast(),
        "writing": library.map(|library| library.is_busy()).unwrap_or(false),
        "version": photomanager_core::VERSION,
        "library": paths.library().display().to_string(),
        "view": window.visible_view(),
        "width": window.width(),
        "height": window.height(),
        "photos": counts.photos,
        "events": counts.events,
        "issues": counts.issues,
        "thumbnails": counts.thumbnails,
        "places": counts.places,
        "people": library.and_then(|library| library.people_known()).map(|(named, at)| serde_json::json!({
            "named": named,
            "fetched_at": at,
        })),
        "dashboard_toast": dashboard.said(),
        "upkeep": dashboard.upkeep_shown().into_iter().map(|(job, state, caption)| serde_json::json!({
            "job": job,
            "state": state,
            "caption": caption,
        })).collect::<Vec<_>>(),
        "scanning": library.map(|library| library.is_scanning()).unwrap_or(false),
    })
    .to_string()
}

fn write_out(target: &str, text: &str) {
    if target.is_empty() {
        println!("{text}");
        return;
    }
    if let Err(error) = std::fs::write(target, text) {
        tracing::error!(%error, target, "cannot write the state");
    }
}

fn snapshot_to_png(window: &Window, target: &Path) -> Result<(), glib::BoolError> {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let width = window.width().max(1);
    let height = window.height().max(1);

    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let Some(node) = snapshot.to_node() else {
        return Err(glib::bool_error!("the window rendered nothing"));
    };
    let renderer = window
        .native()
        .and_then(|native| native.renderer())
        .ok_or_else(|| glib::bool_error!("the window has no renderer"))?;

    renderer.render_texture(&node, None).save_to_png(target)
}
