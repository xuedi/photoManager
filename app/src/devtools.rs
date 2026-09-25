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
    window.add_action_entries([rating, immich_key]);
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
    let applied = preview.applied().map(|(kind, summary)| {
        serde_json::json!({
            "kind": kind.as_str(),
            "batch": summary.batch,
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
            "applied": page.applied().map(|(kind, summary)| serde_json::json!({
                "kind": kind.as_str(),
                "batch": summary.batch,
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
    let tools = window.tools().counted().map(|counted| {
        let tools: serde_json::Map<String, serde_json::Value> = counted
            .tools
            .iter()
            .map(|(key, count)| {
                let count = match count {
                    Ok(count) => serde_json::json!(count),
                    Err(why) => serde_json::json!({ "failed": why }),
                };
                (key.clone(), count)
            })
            .collect();
        let waiting: serde_json::Map<String, serde_json::Value> = counted
            .waiting
            .iter()
            .map(|(key, waiting)| (key.clone(), serde_json::json!(waiting)))
            .collect();
        serde_json::json!({ "photos": counted.photos, "counts": tools, "waiting": waiting })
    });
    let page = window.tools().questions();
    let questions = page.key().map(|key| {
        let asked: Vec<serde_json::Value> = page
            .questions()
            .iter()
            .map(|question| {
                serde_json::json!({
                    "key": question.key,
                    "title": question.title,
                    "photos": question.photos,
                    "apart": question.apart,
                    "sure": question.sure().is_some(),
                    "note": question.note,
                    "kind": format!("{:?}", question.kind).to_lowercase(),
                    "offers": question.offers.iter().map(|offer| offer.words.clone()).collect::<Vec<String>>(),
                    "best": question.offers.first().map(|offer| serde_json::json!({
                        "words": offer.words,
                        "name": offer.place().map(|place| place.name.clone()),
                        "code": offer.place().map(|place| place.code.clone()),
                        "confidence": offer.confidence,
                        "located": offer.located,
                    })),
                    "answer": question.answer.as_ref().map(|answer| match answer {
                        photomanager_core::tools::Answer::Place(place) => serde_json::json!(place.name),
                        photomanager_core::tools::Answer::Pin { lat, lon, near } => {
                            serde_json::json!({ "pin": [lat, lon], "near": near.name })
                        }
                        photomanager_core::tools::Answer::Leave => serde_json::json!("leave"),
                        other => serde_json::json!(other.tells()),
                    }),
                })
            })
            .collect();
        serde_json::json!({
            "tool": key,
            "busy": page.is_busy(),
            "settings": page.settings(),
            "questions": asked,
            "findings": page.findings().iter().map(|finding| serde_json::json!({
                "title": finding.title,
                "detail": finding.detail,
                "rows": finding.rows.len(),
            })).collect::<Vec<_>>(),
            "toast": page.toast(),
            "picking": page.picking().map(|(question, _)| question),
        })
    });
    let vocabulary = window.tools().vocabulary();
    let tags = vocabulary.key().map(|key| {
        let overview = vocabulary.overview();
        serde_json::json!({
            "tool": key,
            "busy": vocabulary.is_busy(),
            "settings": vocabulary.settings(),
            "rules": overview.as_ref().map(|overview| overview.rules.iter().map(|(rule, photos)| serde_json::json!({
                "rule": rule.written(),
                "photos": photos,
            })).collect::<Vec<_>>()),
            "suggestions": overview.as_ref().map(|overview| overview.suggestions.iter().map(|suggestion| serde_json::json!({
                "key": suggestion.key,
                "title": suggestion.title,
                "offer": suggestion.offer,
                "photos": suggestion.photos,
            })).collect::<Vec<_>>()),
            "tree": overview.as_ref().map(|overview| overview.tree.nodes().map(|(path, count)| (path.to_string(), serde_json::json!(count))).collect::<serde_json::Map<String, serde_json::Value>>()),
            "editing": vocabulary.editing(),
            "refused": vocabulary.refused(),
            "toast": vocabulary.toast(),
        })
    });
    let history = window.tools().history();
    let passes: Vec<serde_json::Value> = history
        .passes()
        .iter()
        .map(|pass| {
            serde_json::json!({
                "batch": pass.id,
                "kind": pass.kind.as_str(),
                "title": pass.title,
                "tool": pass.tool,
                "written": pass.written,
                "undoes": pass.undoes,
                "undone_by": pass.undone_by,
                "can_take_back": pass.can_take_back(),
                "changed_since": pass.changed_since,
            })
        })
        .collect();
    let history = serde_json::json!({
        "passes": passes,
        "detail": history.detail().map(|(batch, photos)| serde_json::json!({ "batch": batch, "photos": photos })),
        "taken": history.taken().map(|summary| serde_json::json!({
            "batch": summary.batch,
            "written": summary.written,
            "refused": summary.refused,
            "failed": summary.failed,
        })),
        "toast": history.toast(),
    });
    serde_json::json!({
        "survey": survey,
        "history": history,
        "field": dashboard.field().key(),
        "listed": dashboard.listed_places(),
        "gallery": gallery,
        "photo": photo,
        "scope": scope,
        "tools": tools,
        "questions": questions,
        "vocabulary": tags,
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
