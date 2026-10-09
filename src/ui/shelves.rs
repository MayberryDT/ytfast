use super::motion;
use super::widgets::{
    chip_button, cover, font, icon_button, label, landing_cover, named, play_disc, resting,
    runs_line, runs_text,
};
use super::{CARD, GAP};
use crate::app::Action;
use crate::icons::Icon;
use crate::model::{Item, ItemKind, Shelf, ShelfStyle, Track, format_time};
use crate::theme::Palette;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, RichText, ScrollArea,
    Sense, Ui, Vec2, pos2, vec2,
};
use fastframe_fonts::Weight;

/// What activating an item does: play it (with its shelf as the queue),
/// start its playlist, or open its page.
fn activate(item: &Item, shelf: &Shelf, actions: &mut Vec<Action>) {
    if let Some(track) = &item.track {
        let tracks: Vec<Track> = shelf.items.iter().filter_map(|i| i.track.clone()).collect();
        let start = tracks
            .iter()
            .position(|t| t.video_id == track.video_id)
            .unwrap_or(0);
        actions.push(Action::Play { tracks, start });
    } else if let Some(target) = &item.target {
        actions.push(Action::Activate(target.clone()));
    }
}

fn play_item(item: &Item, shelf: &Shelf, actions: &mut Vec<Action>) {
    match (&item.play, &item.track) {
        (_, Some(_)) => activate(item, shelf, actions),
        (Some(play), None) => actions.push(Action::Activate(play.clone())),
        (None, None) => activate(item, shelf, actions),
    }
}

/// Where an item's cover flies when it's activated: songs and mixes start
/// in Now Playing, as in YouTube Music, and everything else opens its page
/// under the header's cover.
fn destination(item: &Item, play: bool) -> egui::Id {
    let plays = item.track.is_some()
        || (play && item.play.is_some())
        || matches!(item.target, Some(crate::model::Target::Watch { .. }));
    if plays {
        motion::now_playing_site()
    } else {
        motion::header_site()
    }
}

/// Sends an activated item's cover on its way; one that starts music opens
/// Now Playing for it to land in.
fn launch(
    ui: &Ui,
    url: Option<&str>,
    from: Rect,
    radius: f32,
    to: egui::Id,
    actions: &mut Vec<Action>,
) {
    if let Some(url) = url {
        motion::launch_to(ui.ctx(), url, from, radius, to);
    }
    if to == motion::now_playing_site() {
        actions.push(Action::OpenNowPlaying);
    }
}

pub(super) fn shelf_view(
    ui: &mut Ui,
    shelf: &Shelf,
    id: (&str, usize),
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let scroll_id = Id::new(("carousel", id.0, id.1));
    let carousel = matches!(shelf.style, ShelfStyle::Carousel | ShelfStyle::RowCarousel);
    if !shelf.title.is_empty() || shelf.more.is_some() {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                if let Some(strap) = &shelf.strapline {
                    label(ui, strap.to_uppercase(), 12.0, Weight::Medium, p.secondary);
                }
                label(ui, &shelf.title, 24.0, Weight::Bold, p.text);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if carousel {
                    // Named with their shelf, for screen readers and E2E runs.
                    let (right, left) = if shelf.title.is_empty() {
                        ("Scroll right".to_owned(), "Scroll left".to_owned())
                    } else {
                        (
                            format!("{}, scroll right", shelf.title),
                            format!("{}, scroll left", shelf.title),
                        )
                    };
                    if icon_button(ui, Icon::ChevronRight, 18.0, p.text, p, &right).clicked() {
                        nudge(ui, scroll_id, 1.0);
                    }
                    if icon_button(ui, Icon::ChevronLeft, 18.0, p.text, p, &left).clicked() {
                        nudge(ui, scroll_id, -1.0);
                    }
                    ui.add_space(4.0);
                }
                if let Some(more) = &shelf.more
                    && chip_button(ui, "More", None, false, p).clicked()
                {
                    actions.push(Action::Activate(more.clone()));
                }
            });
        });
        ui.add_space(14.0);
    }
    match shelf.style {
        ShelfStyle::Carousel => {
            let out = ScrollArea::horizontal()
                .id_salt(scroll_id)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for item in &shelf.items {
                            card(ui, item, shelf, p, actions);
                        }
                    });
                });
            remember_width(ui, scroll_id, out.inner_rect.width());
            glide(ui, scroll_id, &out, CARD + GAP);
        }
        ShelfStyle::RowCarousel => {
            let out = ScrollArea::horizontal()
                .id_salt(scroll_id)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 24.0;
                        for column in shelf.items.chunks(4) {
                            ui.vertical(|ui| {
                                ui.set_width(380.0);
                                for item in column {
                                    row(ui, item, shelf, false, p, actions);
                                }
                            });
                        }
                    });
                });
            remember_width(ui, scroll_id, out.inner_rect.width());
            glide(ui, scroll_id, &out, 380.0 + 24.0);
        }
        ShelfStyle::List => {
            for item in &shelf.items {
                row(ui, item, shelf, true, p, actions);
            }
        }
        ShelfStyle::Grid => {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(GAP, 24.0);
                for item in &shelf.items {
                    card(ui, item, shelf, p, actions);
                }
            });
        }
        ShelfStyle::Buttons => {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
                for item in &shelf.items {
                    nav_button(ui, item, p, actions);
                }
            });
        }
        ShelfStyle::TopResult => {
            if let Some(item) = shelf.items.first() {
                top_result(ui, item, shelf, p, actions);
            }
        }
    }
}

/// The arrows: glide by most of the visible width.
fn nudge(ui: &Ui, scroll_id: Id, direction: f32) {
    let (width, offset): (f32, f32) = ui.data(|d| {
        (
            d.get_temp(scroll_id.with("width")).unwrap_or(800.0),
            d.get_temp(scroll_id.with("offset")).unwrap_or(0.0),
        )
    });
    // Chained presses add up: start from where the glide is heading.
    let from: f32 = ui
        .data(|d| d.get_temp(scroll_id.with("target")))
        .unwrap_or(offset);
    let target = from + direction * (width - CARD).max(CARD);
    ui.data_mut(|d| d.insert_temp(scroll_id.with("target"), target));
}

/// Moves a carousel towards its glide target, and once the wheel or touchpad
/// lets go, settles it on the nearest card edge, like a physical strip.
fn glide(ui: &Ui, scroll_id: Id, out: &egui::scroll_area::ScrollAreaOutput<()>, step: f32) {
    let ctx = ui.ctx();
    // egui derives the area's id from the salt in its own way: use the one it reports.
    let mut state = out.state;
    ctx.data_mut(|d| d.insert_temp(scroll_id.with("offset"), state.offset.x));
    let max = (out.content_size.x - out.inner_rect.width()).max(0.0);
    let now = ctx.input(|i| i.time);
    let last_key = scroll_id.with("last-scroll");
    let target_key = scroll_id.with("target");
    let hovered = ui.rect_contains_pointer(out.inner_rect);
    if hovered && ctx.input(|i| i.smooth_scroll_delta.x != 0.0) {
        // The hand is on it: follow the hand, settle afterwards.
        ctx.data_mut(|d| {
            d.insert_temp(last_key, now);
            d.remove::<f32>(target_key);
        });
        return;
    }
    let mut target: Option<f32> = ctx.data(|d| d.get_temp(target_key));
    if target.is_none()
        && let Some(last) = ctx.data(|d| d.get_temp::<f64>(last_key))
        && now - last > 0.14
    {
        ctx.data_mut(|d| d.remove::<f64>(last_key));
        target = Some(state.offset.x);
    } else if target.is_none() {
        if ctx.data(|d| d.get_temp::<f64>(last_key)).is_some() {
            ctx.request_repaint();
        }
        return;
    }
    let Some(raw) = target else { return };
    // Card edges, except that the very end may sit between two.
    let snapped = ((raw / step).round() * step).clamp(0.0, max);
    let x = super::motion::drive(ctx, scroll_id.with("glide"), state.offset.x, snapped, 220.0);
    state.offset.x = x;
    state.store(ctx, out.id);
    if x == snapped {
        ctx.data_mut(|d| d.remove::<f32>(target_key));
    } else {
        ctx.data_mut(|d| d.insert_temp(target_key, raw));
    }
}

fn remember_width(ui: &Ui, scroll_id: Id, width: f32) {
    ui.data_mut(|d| d.insert_temp(scroll_id.with("width"), width));
}

fn card(ui: &mut Ui, item: &Item, shelf: &Shelf, p: &Palette, actions: &mut Vec<Action>) {
    let round = item.kind == ItemKind::Artist;
    let (rect, response) = ui.allocate_exact_size(vec2(CARD, CARD + 58.0), Sense::click());
    let hovered = response.hovered();
    // The cover rises a little under the pointer, with weight.
    let lift = motion::lift(ui, response.id, hovered);
    let art = Rect::from_min_size(rect.min, Vec2::splat(CARD))
        .expand(3.0 * lift)
        .translate(vec2(0.0, -3.0 * lift));
    let radius = if round { CARD / 2.0 } else { 6.0 };
    if lift > 0.01 {
        let shadow = egui::epaint::Shadow {
            offset: [0, (8.0 * lift) as i8],
            blur: (22.0 * lift) as u8,
            spread: 0,
            color: p.shadow.gamma_multiply(lift),
        };
        ui.painter()
            .add(shadow.as_shape(art, CornerRadius::same(radius as u8)));
    }
    landing_cover(ui, response.id, art, item.thumbnail.as_deref(), round, 6, p);
    let playable = item.play.is_some() || item.track.is_some();
    let mut play_hit = false;
    if lift > 0.01 && !round {
        ui.painter().rect_filled(
            art,
            CornerRadius::same(6),
            p.overlay.gamma_multiply(0.45 * lift),
        );
    }
    // The play disc pops in rather than appearing.
    let pop = motion::spring(
        ui.ctx(),
        response.id.with("pop"),
        if hovered && playable { 1.0 } else { 0.0 },
        420.0,
    );
    if pop > 0.02 {
        let center = art.right_bottom() - vec2(30.0, 30.0);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        play_hit = hovered && pointer.is_some_and(|pos| pos.distance(center) < 20.0);
        let grow = motion::spring(
            ui.ctx(),
            response.id.with("disc"),
            if play_hit { 1.12 } else { 1.0 },
            500.0,
        );
        play_disc(ui, center, 20.0 * pop * grow, p, play_hit);
    }
    // ⋮ in the cover's corner, rising with the card: its menu.
    let dots_at = art.right_top() + vec2(-22.0, 22.0);
    let dots_hit = super::menu::card_dots(ui, dots_at, lift, hovered, item, p);
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(
                art.left_bottom() + vec2(0.0, 8.0),
                vec2(CARD, 52.0),
            ))
            .layout(Layout::top_down(Align::Min)),
    );
    text_ui.add(
        egui::Label::new(
            RichText::new(&item.title)
                .font(font(Weight::SemiBold, 14.5))
                .color(p.text),
        )
        .truncate()
        .selectable(false),
    );
    text_ui.add(
        egui::Label::new(
            RichText::new(runs_text(&item.subtitle))
                .font(font(Weight::Regular, 13.0))
                .color(p.secondary),
        )
        .truncate()
        .selectable(false),
    );
    let response = named(response, &item.title)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(item.title.as_str());
    if let (true, Some(track)) = (resting(ui, &response), &item.track) {
        actions.push(Action::Prepare(track.video_id.clone()));
    }
    if let Some(track) = &item.track {
        super::audition::hook(ui, &response, track, art, round, p, actions);
    }
    super::menu::on_secondary(
        ui,
        &response,
        || super::menu::subject_of(item, super::menu::Place::List),
        actions,
    );
    if response.clicked()
        && dots_hit
        && let Some(subject) = super::menu::subject_of(item, super::menu::Place::List)
    {
        super::menu::open(ui.ctx(), subject, dots_at + vec2(-16.0, 16.0), actions);
        return;
    }
    if response.clicked() {
        let to = destination(item, play_hit);
        launch(ui, item.thumbnail.as_deref(), art, radius, to, actions);
        if play_hit {
            play_item(item, shelf, actions)
        } else {
            activate(item, shelf, actions)
        }
    }
}

pub(super) fn row(
    ui: &mut Ui,
    item: &Item,
    shelf: &Shelf,
    wide: bool,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let height = 56.0;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), height),
        super::account::row_sense(ui, item),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), p.surface_hover);
    }
    let thumb = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 20.0),
        Vec2::splat(40.0),
    );
    let show_index = item.index.is_some() && (item.thumbnail.is_none() || shelf_is_album(shelf));
    // The cover is where a song row plays without touching a link; E2E runs
    // click it by this name.
    #[cfg(feature = "e2e")]
    if item.track.is_some() {
        crate::e2e::register(ui.ctx(), &format!("Play {}", item.title), thumb);
    }
    if show_index {
        ui.painter().text(
            thumb.center(),
            Align2::CENTER_CENTER,
            item.index.as_deref().unwrap_or(""),
            font(Weight::Medium, 15.0),
            p.secondary,
        );
    } else {
        cover(
            ui,
            thumb,
            item.thumbnail.as_deref(),
            item.kind == ItemKind::Artist,
            4,
            p,
        );
    }
    if response.hovered() && item.track.is_some() && !show_index {
        ui.painter()
            .rect_filled(thumb, CornerRadius::same(4), p.overlay);
        Icon::Play.image(p.text, 20.0).paint_at(
            ui,
            Rect::from_center_size(thumb.center(), Vec2::splat(20.0)),
        );
    }
    let left = thumb.right() + 16.0;
    let duration = item
        .track
        .as_ref()
        .and_then(|t| t.duration)
        .map(|d| format_time(f64::from(d)));
    let right = rect.right()
        - if duration.is_some() { 72.0 } else { 16.0 }
        - super::account::row_reserve(ui, item)
        - super::menu::row_reserve(item);
    let text_rect = Rect::from_min_max(
        pos2(left, rect.top() + 8.0),
        pos2(right, rect.bottom() - 6.0),
    );
    if let (true, Some(track)) = (wide, &item.track) {
        // Title | artists | album, as YouTube Music's song lists.
        let w = text_rect.width();
        let cols = [(0.0, 0.42), (0.44, 0.30), (0.76, 0.24)];
        let mut title_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    text_rect.left_top() + vec2(w * cols[0].0, 9.0),
                    vec2(w * cols[0].1, 24.0),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        label(&mut title_ui, &item.title, 15.0, Weight::Medium, p.text);
        let mut artist_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    text_rect.left_top() + vec2(w * cols[1].0, 10.0),
                    vec2(w * cols[1].1, 24.0),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        if track.artists.is_empty() {
            label(
                &mut artist_ui,
                runs_text(&item.subtitle),
                14.0,
                Weight::Regular,
                p.secondary,
            );
        } else {
            runs_line(&mut artist_ui, &track.artists, 14.0, p, actions);
        }
        if let Some(album) = &track.album {
            let mut album_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        text_rect.left_top() + vec2(w * cols[2].0, 10.0),
                        vec2(w * cols[2].1, 24.0),
                    ))
                    .layout(Layout::left_to_right(Align::Center)),
            );
            runs_line(&mut album_ui, std::slice::from_ref(album), 14.0, p, actions);
        }
    } else {
        let mut text_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        label(&mut text_ui, &item.title, 15.0, Weight::Medium, p.text);
        runs_line(&mut text_ui, &item.subtitle, 13.5, p, actions);
    }
    // The ⋮ takes the length's place while the row is under the pointer.
    let own = super::account::own_entry(ui, item);
    let place = || match own.clone() {
        Some((playlist_id, set_video_id)) => super::menu::Place::Own {
            playlist_id,
            set_video_id,
        },
        None => super::menu::Place::List,
    };
    let dots = super::menu::row(ui, rect, &response, item, place, actions, p);
    if let Some(duration) = duration.filter(|_| !dots) {
        ui.painter().text(
            pos2(rect.right() - 16.0, rect.center().y),
            Align2::RIGHT_CENTER,
            duration,
            font(Weight::Regular, 14.0),
            p.secondary,
        );
    }
    super::account::row_controls(ui, rect, &response, item, p, actions);
    // Links inside the row take their own clicks; the rest of the row plays/opens.
    let response = named(response, &item.title);
    if let (true, Some(track)) = (resting(ui, &response), &item.track) {
        actions.push(Action::Prepare(track.video_id.clone()));
    }
    if let Some(track) = &item.track {
        super::audition::hook(ui, &response, track, thumb, false, p, actions);
    }
    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        // A song's cover flies from its row to Now Playing.
        let thumb = Rect::from_min_size(
            pos2(rect.left() + 8.0, rect.center().y - 20.0),
            Vec2::splat(40.0),
        );
        let url = item
            .thumbnail
            .as_deref()
            .or(item.track.as_ref().and_then(|t| t.thumbnail.as_deref()));
        launch(ui, url, thumb, 4.0, destination(item, false), actions);
        activate(item, shelf, actions);
    }
}

fn shelf_is_album(shelf: &Shelf) -> bool {
    shelf.items.iter().all(|i| i.index.is_some())
}

fn nav_button(ui: &mut Ui, item: &Item, p: &Palette, actions: &mut Vec<Action>) {
    let (rect, response) = ui.allocate_exact_size(vec2(196.0, 48.0), Sense::click());
    let fill = if response.hovered() {
        p.surface_hover
    } else {
        p.surface
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    if let Some(argb) = item.stripe {
        // YouTube Music's own colour for the mood (content, like cover art).
        let [_, r, g, b] = argb.to_be_bytes();
        let stripe = Rect::from_min_size(rect.min, vec2(6.0, rect.height()));
        ui.painter().rect_filled(
            stripe,
            CornerRadius {
                nw: 8,
                sw: 8,
                ne: 0,
                se: 0,
            },
            Color32::from_rgb(r, g, b),
        );
    }
    let galley = ui.painter().layout(
        item.title.clone(),
        font(Weight::Medium, 14.0),
        p.text,
        rect.width() - 32.0,
    );
    ui.painter().galley(
        pos2(rect.left() + 18.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    if named(response, &item.title)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
        && let Some(target) = &item.target
    {
        actions.push(Action::Activate(target.clone()));
    }
}

fn top_result(ui: &mut Ui, item: &Item, shelf: &Shelf, p: &Palette, actions: &mut Vec<Action>) {
    Frame::new()
        .fill(p.panel)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            ui.horizontal(|ui| {
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(112.0), Sense::click());
                super::menu::on_secondary(
                    ui,
                    &response,
                    || super::menu::subject_of(item, super::menu::Place::List),
                    actions,
                );
                cover(
                    ui,
                    rect,
                    item.thumbnail.as_deref(),
                    item.kind == ItemKind::Artist,
                    6,
                    p,
                );
                #[cfg(feature = "e2e")]
                if item.track.is_some() {
                    crate::e2e::register(ui.ctx(), &format!("Play {}", item.title), rect);
                }
                if let Some(track) = &item.track {
                    let round = item.kind == ItemKind::Artist;
                    super::audition::hook(ui, &response, track, rect, round, p, actions);
                }
                let thumbnail = item.thumbnail.as_deref();
                if named(response, &item.title)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    launch(ui, thumbnail, rect, 6.0, destination(item, false), actions);
                    activate(item, shelf, actions);
                }
                ui.add_space(20.0);
                ui.vertical(|ui| {
                    ui.add_space(10.0);
                    let title = ui.add(
                        egui::Label::new(
                            RichText::new(&item.title)
                                .font(font(Weight::Bold, 26.0))
                                .color(p.text),
                        )
                        .truncate()
                        .sense(Sense::click())
                        .selectable(false),
                    );
                    if title
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        launch(ui, thumbnail, rect, 6.0, destination(item, false), actions);
                        activate(item, shelf, actions);
                    }
                    runs_line(ui, &item.subtitle, 14.0, p, actions);
                    ui.add_space(10.0);
                    if (item.play.is_some() || item.track.is_some())
                        && chip_button(ui, "Play", Some(Icon::Play), true, p).clicked()
                    {
                        launch(ui, thumbnail, rect, 6.0, destination(item, true), actions);
                        play_item(item, shelf, actions);
                    }
                });
            });
        });
}
