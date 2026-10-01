use super::shelves::shelf_view;
use super::widgets::{chip, cover, font, label, pill, resting, runs_line};
use super::{CARD, GAP};
use crate::app::{Action, App, LibraryTab, PageState, View};
use crate::icons::Icon;
use crate::model::{Header, Page, Target, Track};
use crate::theme::Palette;
use egui::{CornerRadius, Frame, Margin, Rect, RichText, ScrollArea, Sense, Ui, Vec2, pos2, vec2};
use fastframe_fonts::Weight;

pub(super) fn content(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let target = app.view.target();
    let key = target.key();
    let mut scroll = ScrollArea::vertical()
        .id_salt(("page", &key))
        .auto_shrink([false, false]);
    if std::mem::take(&mut app.scroll_to_top) {
        scroll = scroll.vertical_scroll_offset(0.0);
    }
    let app: &App = app;
    scroll.show_viewport(ui, |ui, viewport| {
        Frame::new()
            .inner_margin(Margin {
                left: 32,
                right: 32,
                top: 8,
                bottom: 48,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let View::Library(tab) = &app.view {
                    library_tabs(ui, *tab, p, actions);
                    ui.add_space(20.0);
                }
                match app.page_state(&target) {
                    Some(state) => page_view(ui, state, &key, p, actions),
                    None => skeleton(ui, p),
                }
            });
        // Near the bottom: ask for more.
        let near_bottom = viewport.max.y > ui.min_rect().height() - 900.0;
        if near_bottom && let Some(state) = app.page_state(&target) {
            request_more(state, &key, actions);
        }
    });
}

fn request_more(state: &PageState, key: &str, actions: &mut Vec<Action>) {
    let Some(page) = &state.page else { return };
    if state.cached {
        return;
    }
    let search = matches!(state.target, Target::Search { .. });
    if let Some(token) = &page.continuation {
        if !state.more_loading.contains(&None) {
            actions.push(Action::More {
                key: key.to_owned(),
                token: token.clone(),
                search,
                shelf: None,
            });
        }
        return;
    }
    // Long lists (playlists, library songs): the last shelf grows.
    if let Some((i, shelf)) = page
        .shelves
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| s.continuation.is_some())
        && !state.more_loading.contains(&Some(i))
    {
        actions.push(Action::More {
            key: key.to_owned(),
            token: shelf.continuation.clone().unwrap_or_default(),
            search,
            shelf: Some(i),
        });
    }
}

fn library_tabs(ui: &mut Ui, current: LibraryTab, p: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        for tab in LibraryTab::ALL {
            if chip(ui, tab.label(), tab == current, p).clicked() && tab != current {
                actions.push(Action::Open(View::Library(tab)));
            }
            ui.add_space(4.0);
        }
        super::account::library_actions(ui, current, p, actions);
    });
}

fn page_view(ui: &mut Ui, state: &PageState, key: &str, p: &Palette, actions: &mut Vec<Action>) {
    let Some(page) = &state.page else {
        if let Some(error) = &state.error {
            notice(ui, "Can't load this page", error, Some(key), p, actions);
        } else {
            skeleton(ui, p);
        }
        return;
    };
    if let Some(error) = &state.error {
        // Showing the saved copy: say so and offer a retry.
        ui.horizontal(|ui| {
            Icon::Alert.image(p.warning, 16.0).paint_at(
                ui,
                Rect::from_min_size(ui.cursor().min + vec2(0.0, 2.0), Vec2::splat(16.0)),
            );
            ui.add_space(22.0);
            label(
                ui,
                format!("Showing the saved copy. {error}"),
                13.0,
                Weight::Regular,
                p.secondary,
            );
            if ui.link("Retry").clicked() {
                actions.push(Action::Retry(key.to_owned()));
            }
        });
        ui.add_space(12.0);
    }
    if let Target::Search { query, .. } = &state.target {
        label(
            ui,
            format!("Results for “{query}”"),
            14.0,
            Weight::Regular,
            p.secondary,
        );
        ui.add_space(8.0);
    }
    if let Some(header) = &page.header {
        page_header(ui, header, page, p, actions);
        ui.add_space(28.0);
    }
    if !page.chips.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for c in &page.chips {
                if chip(ui, &c.text, c.selected, p).clicked()
                    && let Some(target) = &c.target
                {
                    actions.push(Action::Activate(target.clone()));
                }
                ui.add_space(4.0);
            }
        });
        ui.add_space(20.0);
    }
    if page.shelves.is_empty() {
        let message = page
            .message
            .clone()
            .unwrap_or_else(|| "Nothing here yet.".into());
        label(ui, message, 15.0, Weight::Regular, p.secondary);
        return;
    }
    for (i, shelf) in page.shelves.iter().enumerate() {
        shelf_view(ui, shelf, (key, i), p, actions);
        if state.more_loading.contains(&Some(i)) {
            ui.add(egui::Spinner::new().size(20.0).color(p.secondary));
        }
        ui.add_space(36.0);
    }
    if state.more_loading.contains(&None) {
        skeleton_shelf(ui, p);
    }
}

fn notice(
    ui: &mut Ui,
    title: &str,
    detail: &str,
    retry: Option<&str>,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        Icon::Alert.image(p.secondary, 36.0).paint_at(
            ui,
            Rect::from_center_size(
                ui.cursor().min + vec2(ui.available_width() / 2.0, 18.0),
                Vec2::splat(36.0),
            ),
        );
        ui.add_space(48.0);
        ui.label(
            RichText::new(title)
                .font(font(Weight::SemiBold, 18.0))
                .color(p.text),
        );
        ui.label(
            RichText::new(detail)
                .font(font(Weight::Regular, 14.0))
                .color(p.secondary),
        );
        ui.add_space(12.0);
        if let Some(key) = retry
            && pill(ui, "Retry", Some(Icon::Refresh), true, p).clicked()
        {
            actions.push(Action::Retry(key.to_owned()));
        }
    });
}

/// Placeholders in the shape of the content on its way.
fn skeleton(ui: &mut Ui, p: &Palette) {
    for _ in 0..3 {
        skeleton_shelf(ui, p);
        ui.add_space(36.0);
    }
}

pub(super) fn skeleton_shelf(ui: &mut Ui, p: &Palette) {
    let pulse = (ui.input(|i| i.time) * 2.0).sin() as f32 * 0.5 + 0.5;
    let fill = p.surface.lerp_to_gamma(p.surface_hover, pulse);
    let (title, _) = ui.allocate_exact_size(vec2(220.0, 26.0), Sense::hover());
    ui.painter().rect_filled(title, CornerRadius::same(6), fill);
    ui.add_space(14.0);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), CARD + 52.0), Sense::hover());
    let mut x = row.left();
    while x + CARD <= row.right() {
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(x, row.top()), Vec2::splat(CARD)),
            CornerRadius::same(6),
            fill,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(x, row.top() + CARD + 10.0), vec2(CARD * 0.8, 12.0)),
            CornerRadius::same(4),
            fill,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(x, row.top() + CARD + 30.0), vec2(CARD * 0.55, 10.0)),
            CornerRadius::same(4),
            fill,
        );
        x += CARD + GAP;
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

fn page_header(ui: &mut Ui, h: &Header, page: &Page, p: &Palette, actions: &mut Vec<Action>) {
    if h.thumbnail.is_none() && h.play.is_none() && h.subtitle.is_empty() {
        label(ui, &h.title, 32.0, Weight::Bold, p.text);
        return;
    }
    ui.horizontal_top(|ui| {
        let size = 232.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        cover(ui, rect, h.thumbnail.as_deref(), h.round, 8, p);
        ui.add_space(28.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(720.0));
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    RichText::new(&h.title)
                        .font(font(Weight::Bold, 34.0))
                        .color(p.text),
                )
                .wrap()
                .selectable(false),
            );
            ui.add_space(6.0);
            runs_line(ui, &h.subtitle, 15.0, p, actions);
            if !h.second_subtitle.is_empty() {
                label(ui, &h.second_subtitle, 14.0, Weight::Regular, p.secondary);
            }
            if let Some(description) = &h.description {
                ui.add_space(8.0);
                let short: String = description.chars().take(320).collect();
                let short = if short.len() < description.len() {
                    format!("{short}…")
                } else {
                    short
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(short)
                            .font(font(Weight::Regular, 14.0))
                            .color(p.secondary),
                    )
                    .wrap(),
                )
                .on_hover_text(description.as_str());
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let tracks: Vec<Track> = page
                    .shelves
                    .iter()
                    .flat_map(|s| s.items.iter())
                    .filter_map(|i| i.track.clone())
                    .collect();
                if h.play.is_some() || !tracks.is_empty() {
                    let play = pill(ui, "Play", Some(Icon::Play), true, p);
                    if resting(ui, &play)
                        && let Some(first) = tracks.first()
                    {
                        actions.push(Action::Prepare(first.video_id.clone()));
                    }
                    if play.clicked() {
                        match &h.play {
                            Some(play) => actions.push(Action::Activate(play.clone())),
                            None => actions.push(Action::Play {
                                tracks: tracks.clone(),
                                start: 0,
                            }),
                        }
                    }
                    ui.add_space(8.0);
                }
                if let Some(shuffle) = &h.shuffle {
                    if pill(ui, "Shuffle", Some(Icon::Shuffle), false, p).clicked() {
                        actions.push(Action::Activate(shuffle.clone()));
                    }
                    ui.add_space(8.0);
                }
                if let Some(radio) = &h.radio
                    && pill(ui, "Radio", Some(Icon::Radio), false, p).clicked()
                {
                    actions.push(Action::Activate(radio.clone()));
                }
                ui.add_space(8.0);
                super::account::header_actions(ui, h, p, actions);
            });
        });
    });
}
