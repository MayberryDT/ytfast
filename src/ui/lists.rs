//! Local list views never change the server's order or entry identities.
use crate::account::{AccountAction, Dialog};
use crate::backend::Command;
use crate::{
    app::Action,
    model::{Item, Shelf, Track},
    theme::Palette,
};
use egui::{Id, Ui};
use std::collections::HashSet;

#[derive(Clone, Default)]
struct View {
    query: String,
    sort: usize,
    selected: HashSet<String>,
    anchor: Option<String>,
}
#[derive(Clone)]
struct Rows {
    id: Id,
    keys: std::sync::Arc<Vec<String>>,
}
#[derive(Clone)]
struct Cached {
    source: std::sync::Arc<Shelf>,
    query: String,
    sort: usize,
    displayed: std::sync::Arc<Shelf>,
    keys: std::sync::Arc<Vec<String>>,
}
fn current() -> Id {
    Id::new("list-current")
}
fn key(item: &Item) -> String {
    item.track
        .as_ref()
        .and_then(|t| t.set_video_id.clone())
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                item.index.as_deref().unwrap_or(""),
                item.track
                    .as_ref()
                    .map_or(item.title.as_str(), |t| &t.video_id)
            )
        })
}
pub(super) fn selected(ui: &Ui, item: &Item) -> bool {
    ui.data(|d| {
        d.get_temp::<Rows>(current())
            .and_then(|r| d.get_temp::<View>(r.id))
            .is_some_and(|v| v.selected.contains(&key(item)))
    })
}
pub(super) fn pick(ui: &Ui, item: &Item, response: &egui::Response) -> bool {
    if !response.clicked() || item.track.is_none() {
        return false;
    }
    let Some(rows) = ui.data(|d| d.get_temp::<Rows>(current())) else {
        return false;
    };
    let mut view = ui.data(|d| d.get_temp::<View>(rows.id)).unwrap_or_default();
    let modifiers = ui.input(|i| i.modifiers);
    let extend = modifiers.ctrl || modifiers.command;
    let k = key(item);
    if modifiers.shift {
        if !extend {
            view.selected.clear();
        }
        let a = view
            .anchor
            .as_ref()
            .and_then(|a| rows.keys.iter().position(|k| k == a));
        let b = rows.keys.iter().position(|v| v == &k);
        if let (Some(a), Some(b)) = (a, b) {
            view.selected
                .extend(rows.keys[a.min(b)..=a.max(b)].iter().cloned());
        } else {
            view.selected.insert(k.clone());
            view.anchor = Some(k);
        }
    } else if extend {
        if !view.selected.remove(&k) {
            view.selected.insert(k.clone());
        }
        view.anchor = Some(k);
    } else {
        view.selected.clear();
        view.anchor = Some(k);
    }
    ui.data_mut(|d| d.insert_temp(rows.id, view));
    modifiers.shift || extend
}
pub(super) fn show(
    ui: &mut Ui,
    shelf: &Shelf,
    scope: (&str, usize),
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let id = Id::new(("list-view", scope));
    let mut view = ui.data(|d| d.get_temp::<View>(id)).unwrap_or_default();
    let previous_height = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = 32.0;
    let tracks = shelf.items.iter().filter(|i| i.track.is_some()).count();
    if tracks > 0 {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut view.query)
                    .hint_text("Find in playlist")
                    .desired_width(210.0),
            );
            #[cfg(feature = "e2e")]
            crate::e2e::register(ui.ctx(), "Find in playlist", response.rect);
            let _ = response;
            let sort_menu = egui::ComboBox::from_id_salt(id.with("sort"))
                .selected_text(["Playlist order", "Title", "Artist", "Duration"][view.sort])
                .show_ui(ui, |ui| {
                    for (i, title) in ["Playlist order", "Title", "Artist", "Duration"]
                        .iter()
                        .enumerate()
                    {
                        let response = ui.selectable_value(&mut view.sort, i, *title);
                        #[cfg(feature = "e2e")]
                        crate::e2e::register(ui.ctx(), title, response.rect);
                        let _ = response;
                    }
                });
            #[cfg(feature = "e2e")]
            crate::e2e::register(ui.ctx(), "Sort songs", sort_menu.response.rect);
            let _ = sort_menu;
        });
        ui.add_space(8.0);
        if shelf.continuation.is_some() {
            ui.label("Search and sort cover loaded songs. Scroll down to load more.");
        }
    }
    let needle = view.query.trim().to_lowercase();
    let cached = ui.data(|d| d.get_temp::<Cached>(id.with("rows")));
    let cache = match cached {
        Some(cache)
            if cache.source.as_ref() == shelf
                && cache.query == needle
                && cache.sort == view.sort =>
        {
            cache
        }
        _ => {
            let mut displayed = shelf.clone();
            if !needle.is_empty() || view.sort != 0 {
                displayed.items.retain(|i| {
                    needle.is_empty()
                        || format!("{} {}", i.title, super::widgets::runs_text(&i.subtitle))
                            .to_lowercase()
                            .contains(&needle)
                });
                match view.sort {
                    1 => displayed
                        .items
                        .sort_by_cached_key(|i| i.title.to_lowercase()),
                    2 => displayed.items.sort_by_cached_key(|i| {
                        super::widgets::runs_text(&i.subtitle).to_lowercase()
                    }),
                    3 => displayed.items.sort_by_key(|i| {
                        i.track
                            .as_ref()
                            .and_then(|t| t.duration)
                            .unwrap_or(u32::MAX)
                    }),
                    _ => {}
                }
            }
            let keys = std::sync::Arc::new(displayed.items.iter().map(key).collect());
            let cache = Cached {
                source: std::sync::Arc::new(shelf.clone()),
                query: needle,
                sort: view.sort,
                displayed: std::sync::Arc::new(displayed),
                keys,
            };
            ui.data_mut(|d| d.insert_temp(id.with("rows"), cache.clone()));
            cache
        }
    };
    let displayed = cache.displayed;
    let keys = cache.keys;
    let present: HashSet<_> = keys.iter().collect();
    view.selected.retain(|k| present.contains(k));
    let chosen: Vec<Track> = displayed
        .items
        .iter()
        .filter(|i| view.selected.contains(&key(i)))
        .filter_map(|i| i.track.clone())
        .collect();
    if !chosen.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} selected", chosen.len()));
            if super::widgets::named(
                super::widgets::chip_button(ui, "Play next", None, false, p),
                "Play next",
            )
            .clicked()
            {
                actions.push(Action::Command(Command::PlayNext(chosen.clone())));
            }
            if super::widgets::named(
                super::widgets::chip_button(ui, "Add to queue", None, false, p),
                "Add to queue",
            )
            .clicked()
            {
                actions.push(Action::Command(Command::AddToQueue(chosen.clone())));
            }
            if super::widgets::named(
                super::widgets::chip_button(ui, "Add to playlist", None, false, p),
                "Add to playlist",
            )
            .clicked()
            {
                actions.push(Action::Account(AccountAction::Dialog(Some(
                    Dialog::AddToPlaylist {
                        tracks: chosen,
                        filter: String::new(),
                        selected: 0,
                    },
                ))));
            }
            if super::widgets::named(
                super::widgets::chip_button(ui, "Clear selection", None, false, p),
                "Clear selection",
            )
            .clicked()
            {
                view.selected.clear();
            }
        });
    }
    ui.spacing_mut().interact_size.y = previous_height;
    ui.add_space(8.0);
    ui.data_mut(|d| {
        d.insert_temp(id, view);
        d.insert_temp(current(), Rows { id, keys });
    });
    #[cfg(feature = "e2e")]
    crate::e2e::probe(
        "list-view",
        serde_json::json!({"count":displayed.items.len(), "first":displayed.items.first().map(|i| &i.title), "selected":ui.data(|d| d.get_temp::<View>(id)).map_or(0, |v|v.selected.len())}),
    );
    if displayed.items.is_empty() {
        ui.label("No matching songs");
    }
    let spacing = ui.spacing().item_spacing.y;
    super::widgets::virtual_rows(ui, displayed.items.len(), 56.0 + spacing, |ui, index| {
        ui.push_id(key(&displayed.items[index]), |ui| {
            super::shelves::row(ui, &displayed.items[index], &displayed, true, p, actions);
        });
        ui.add_space(spacing);
    });
    ui.data_mut(|d| d.remove::<Rows>(current()));
}
