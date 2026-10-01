use super::motion;
use super::pages::skeleton_shelf;
use super::shelves::shelf_view;
use super::widgets::{cover, font, label, landing_cover, named, pill, runs_line, track_line};
use crate::app::{Action, App, NowPlayingTab};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{ShelfStyle, Target, format_time};
use crate::theme::Palette;
use egui::{
    Align, Align2, CornerRadius, Id, Layout, Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};
use fastframe_fonts::Weight;

pub(super) fn now_playing(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    // Now Playing takes its colour from the cover: a wash in the cover's
    // hue, and text and accent chosen to read on it. It moves to the next
    // cover's colours when the song changes.
    let (wash, moving) = app.cover_fade.wash(app.cover_colors.as_ref(), p);
    wash.paint(ui.painter(), ui.max_rect());
    if moving {
        ui.ctx().request_repaint();
    }
    let tinted = wash.palette(p);
    let p = &tinted;
    let area = ui.max_rect().shrink2(vec2(40.0, 24.0));
    let side = 440.0_f32.min(area.width() * 0.45);
    let art_area = Rect::from_min_max(area.min, pos2(area.right() - side - 40.0, area.bottom()));
    let side_area = Rect::from_min_max(pos2(area.right() - side, area.top()), area.max);
    let current = app.playback.index.and_then(|i| app.queue.get(i));

    // The cover, as large as fits above the title, artists, account
    // controls, format and the jump to the most replayed part.
    let below_height = 200.0;
    let size = art_area
        .width()
        .min(art_area.height() - below_height)
        .max(120.0);
    let art = Rect::from_center_size(
        pos2(art_area.center().x, art_area.top() + size / 2.0 + 8.0),
        Vec2::splat(size),
    );
    let url = current.and_then(|t| t.thumbnail.as_deref());
    landing_cover(ui, motion::now_playing_site(), art, url, false, 8, p);
    if let Some(url) = url {
        motion::origin(ui.ctx(), "now-playing", url, art, 8.0);
    }
    // A double click on the cover opens Stage.
    let art_response = ui.interact(art, Id::new("now-playing-art"), Sense::click());
    if named(art_response, "Now playing cover")
        .on_hover_text("Double-click for Stage (F)")
        .double_clicked()
    {
        actions.push(Action::Stage(true));
    }
    if let Some(track) = current {
        let mut below = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(art.left(), art.bottom() + 16.0),
                    pos2(art.right(), art_area.bottom()),
                ))
                .layout(Layout::top_down(Align::Center)),
        );
        label(&mut below, &track.title, 22.0, Weight::Bold, p.text);
        // Artist and album link to their pages, as in the player bar.
        let line = track_line(track);
        let width: f32 = line
            .iter()
            .map(|r| {
                below
                    .painter()
                    .layout_no_wrap(r.text.clone(), font(Weight::Regular, 15.0), p.secondary)
                    .size()
                    .x
            })
            .sum();
        let mut centered = below.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            pos2(
                art.center().x - width.min(art.width()) / 2.0,
                below.cursor().top(),
            ),
            vec2(width.min(art.width()) + 2.0, 22.0),
        )));
        runs_line(&mut centered, &line, 15.0, p, actions);
        below.add_space(24.0);
        super::account::now_playing_controls(&mut below, track, p, actions);
        if let Some(format) = &app.playback.format {
            label(&mut below, format, 12.0, Weight::Regular, p.dim);
        }
        // Where YouTube knows the most replayed part, a jump straight to it.
        if app.current_heat().is_some_and(|h| h.peak.is_some()) {
            below.add_space(10.0);
            if super::widgets::pill(&mut below, "Jump to the most replayed part", None, false, p)
                .clicked()
            {
                actions.push(Action::JumpToPeak);
            }
        }
    }

    let mut side_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(side_area)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut side_ui;
    ui.horizontal(|ui| {
        for (tab, text) in [
            (NowPlayingTab::UpNext, "UP NEXT"),
            (NowPlayingTab::Lyrics, "LYRICS"),
            (NowPlayingTab::Related, "RELATED"),
        ] {
            let enabled = match tab {
                NowPlayingTab::UpNext => true,
                // LRCLIB may have lyrics where YouTube Music has none.
                NowPlayingTab::Lyrics => current.is_some(),
                NowPlayingTab::Related => app.playback.related.is_some(),
            };
            let selected = app.now_playing_tab == tab;
            let color = if selected {
                p.text
            } else if enabled {
                p.secondary
            } else {
                p.dim
            };
            let (rect, response) = ui.allocate_exact_size(
                vec2(side / 3.0 - 6.0, 44.0),
                if enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                font(Weight::SemiBold, 13.5),
                color,
            );
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 1.0,
                Stroke::new(
                    if selected { 2.0 } else { 1.0 },
                    if selected { p.text } else { p.outline },
                ),
            );
            if named(response, text).clicked() {
                actions.push(Action::NowPlayingTab(tab));
            }
        }
    });
    ui.add_space(8.0);
    match app.now_playing_tab {
        NowPlayingTab::UpNext => up_next(app, ui, p, actions),
        NowPlayingTab::Lyrics => super::lyrics::lyrics(app, ui, p, actions),
        NowPlayingTab::Related => related(app, ui, p, actions),
    }
}

/// The queue in play order. Each row plays on click; on hover it shows a
/// handle to drag it elsewhere and a button to remove it.
fn up_next(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let current = app.playback.index;
    let upcoming = current.map_or(0, |c| app.queue.len().saturating_sub(c + 1));
    ui.horizontal(|ui| {
        let mut autoplay = app.playback.autoplay;
        if ui
            .checkbox(
                &mut autoplay,
                RichText::new("Autoplay")
                    .font(font(Weight::Medium, 14.0))
                    .color(p.text),
            )
            .on_hover_text("Add similar songs when the queue ends")
            .changed()
        {
            actions.push(Action::Command(Command::Autoplay(autoplay)));
        }
        if upcoming > 0 {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, "Clear", None, false, p)
                    .on_hover_text("Remove the songs after this one")
                    .clicked()
                {
                    actions.push(Action::Command(Command::ClearUpcoming));
                }
            });
        }
    });
    ui.add_space(6.0);
    ScrollArea::vertical()
        .id_salt("up-next")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut rows: Vec<Rect> = Vec::with_capacity(app.queue.len());
            let mut dragging = None;
            let mut dropped = None;
            for (i, track) in app.queue.iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
                rows.push(rect);
                let is_current = current == Some(i);
                let pointer_here = ui.rect_contains_pointer(rect);
                let handle = Rect::from_min_size(rect.min, vec2(28.0, rect.height()));
                let handle_response = named(
                    ui.interact(handle, Id::new(("up-next-handle", i)), Sense::drag()),
                    &format!("Reorder {}", track.title),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);
                let held = handle_response.dragged();
                if held {
                    dragging = Some(i);
                }
                if handle_response.drag_stopped() {
                    dropped = Some(i);
                }
                let tools = (pointer_here && !ui.input(|i| i.pointer.any_down())) || held;
                if is_current || held {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), p.surface_active);
                    if is_current
                        && app.now_playing
                        && ui.ctx().memory(|m| {
                            m.data.get_temp::<usize>(Id::new("up-next-scrolled")) != Some(i)
                        })
                    {
                        ui.scroll_to_rect(rect, Some(Align::Center));
                        ui.ctx()
                            .memory_mut(|m| m.data.insert_temp(Id::new("up-next-scrolled"), i));
                    }
                } else if pointer_here {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), p.surface_hover);
                }
                if tools {
                    Icon::Grip.image(p.secondary, 16.0).paint_at(
                        ui,
                        Rect::from_center_size(handle.center(), Vec2::splat(16.0)),
                    );
                }
                let thumb = Rect::from_min_size(
                    pos2(handle.right(), rect.center().y - 20.0),
                    Vec2::splat(40.0),
                );
                cover(ui, thumb, track.thumbnail.as_deref(), false, 4, p);
                if is_current && app.playback.playing {
                    ui.painter()
                        .rect_filled(thumb, CornerRadius::same(4), p.overlay);
                    Icon::Volume.image(p.text, 18.0).paint_at(
                        ui,
                        Rect::from_center_size(thumb.center(), Vec2::splat(18.0)),
                    );
                }
                let mut text = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_max(
                            pos2(thumb.right() + 12.0, rect.top() + 8.0),
                            pos2(rect.right() - 56.0, rect.bottom()),
                        ))
                        .layout(Layout::top_down(Align::Min)),
                );
                label(&mut text, &track.title, 14.5, Weight::Medium, p.text);
                runs_line(&mut text, &track_line(track), 13.0, p, actions);
                // The playing song can't be removed; the others show a remove
                // button in place of their length on hover.
                let remove = (!is_current).then(|| {
                    let area = Rect::from_center_size(
                        pos2(rect.right() - 24.0, rect.center().y),
                        Vec2::splat(32.0),
                    );
                    named(
                        ui.interact(area, Id::new(("up-next-remove", i)), Sense::click()),
                        &format!("Remove {}", track.title),
                    )
                });
                match &remove {
                    Some(button) if tools => {
                        if button.hovered() {
                            ui.painter().circle_filled(
                                button.rect.center(),
                                16.0,
                                p.surface_active,
                            );
                        }
                        Icon::Close.image(p.text, 16.0).paint_at(
                            ui,
                            Rect::from_center_size(button.rect.center(), Vec2::splat(16.0)),
                        );
                    }
                    _ => {
                        if let Some(d) = track.duration {
                            ui.painter().text(
                                pos2(rect.right() - 10.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                format_time(f64::from(d)),
                                font(Weight::Regular, 13.0),
                                p.secondary,
                            );
                        }
                    }
                }
                if remove.is_some_and(|b| b.on_hover_text("Remove from queue").clicked()) {
                    actions.push(Action::Command(Command::RemoveFromQueue(i)));
                }
                if named(response, &track.title)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    && !is_current
                {
                    actions.push(Action::Command(Command::JumpTo(i)));
                }
            }
            // A row being dragged: a line shows where it will land.
            let Some(from) = dragging.or(dropped) else {
                return;
            };
            let Some(pointer) = ui.ctx().pointer_latest_pos() else {
                return;
            };
            let gap = rows
                .iter()
                .position(|r| pointer.y < r.center().y)
                .unwrap_or(rows.len());
            if dropped.is_some() {
                let to = if gap > from { gap - 1 } else { gap };
                if to != from {
                    actions.push(Action::Command(Command::MoveInQueue { from, to }));
                }
                return;
            }
            let y = match rows.get(gap) {
                Some(row) => row.top(),
                None => rows.last().map_or(pointer.y, |r| r.bottom()),
            };
            if gap != from && gap != from + 1 {
                let width = rows.first().map_or(0.0..=0.0, |r| r.left()..=r.right());
                ui.painter().hline(width, y, Stroke::new(2.0, p.accent));
            }
            // Near the edges, the list scrolls to reach further rows.
            let clip = ui.clip_rect();
            if pointer.y < clip.top() + 40.0 {
                ui.scroll_with_delta(vec2(0.0, 8.0));
            } else if pointer.y > clip.bottom() - 40.0 {
                ui.scroll_with_delta(vec2(0.0, -8.0));
            }
            ui.ctx().request_repaint();
        });
}

fn related(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let Some(id) = &app.playback.related else {
        label(
            ui,
            "Nothing related to show.",
            15.0,
            Weight::Regular,
            p.secondary,
        );
        return;
    };
    let target = Target::browse(id.clone());
    let key = target.key();
    match app.page_state(&target) {
        None => {
            actions.push(Action::Load(target));
            skeleton_shelf(ui, p);
        }
        Some(state) => {
            ScrollArea::vertical()
                .id_salt(("related", id))
                .auto_shrink([false, false])
                .show(ui, |ui| match &state.page {
                    Some(page) => {
                        for (i, shelf) in page.shelves.iter().enumerate() {
                            let mut narrow = shelf.clone();
                            if narrow.style == ShelfStyle::Carousel {
                                narrow.style = ShelfStyle::List;
                            }
                            shelf_view(ui, &narrow, (&key, i), p, actions);
                            ui.add_space(24.0);
                        }
                    }
                    None if state.loading => skeleton_shelf(ui, p),
                    None => {
                        label(
                            ui,
                            state.error.clone().unwrap_or_default(),
                            14.0,
                            Weight::Regular,
                            p.secondary,
                        );
                    }
                });
        }
    }
}
