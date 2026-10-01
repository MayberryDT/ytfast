//! The interface, laid out like YouTube Music: navigation on the left, the
//! search bar on top, the page in the middle and the player bar below.
//! Views read [`App`] and push [`Action`]s.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Layout, Margin, Rect, RichText,
    ScrollArea, Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use fastframe_fonts::Weight;

use crate::app::{Action, App, LibraryTab, NowPlayingTab, PageState, View};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{
    Account, Header, Item, ItemKind, Page, Repeat, Run, Shelf, ShelfStyle, Target, Track,
    format_time,
};
use crate::theme::Palette;

const SIDEBAR: f32 = 232.0;
const TOPBAR: f32 = 64.0;
const PLAYER: f32 = 76.0;
const CARD: f32 = 176.0;
const GAP: f32 = 16.0;

pub fn draw(app: &mut App, ui: &mut Ui, actions: &mut Vec<Action>) {
    let p = app.palette.clone();
    keyboard(app, ui, actions);
    if !app.queue.is_empty() {
        egui::Panel::bottom("player")
            .exact_size(PLAYER)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(p.panel))
            .show(ui, |ui| player_bar(app, ui, &p, actions));
    }
    egui::Panel::left("navigation")
        .exact_size(SIDEBAR)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(p.window).inner_margin(Margin {
            left: 12,
            right: 12,
            top: 14,
            bottom: 8,
        }))
        .show(ui, |ui| sidebar(app, ui, &p, actions));
    egui::Panel::top("top")
        .exact_size(TOPBAR)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(p.window)
                .inner_margin(Margin::symmetric(24, 12)),
        )
        .show(ui, |ui| top_bar(app, ui, &p, actions));
    egui::CentralPanel::no_frame().show(ui, |ui| {
        ui.painter().rect_filled(ui.max_rect(), 0.0, p.window);
        if app.now_playing {
            now_playing(app, ui, &p, actions);
        } else {
            content(app, ui, &p, actions);
        }
        errors(app, ui, &p, actions);
    });
}

fn keyboard(app: &App, ui: &Ui, actions: &mut Vec<Action>) {
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    ui.input(|i| {
        if !typing && i.key_pressed(egui::Key::Space) && !app.queue.is_empty() {
            actions.push(Action::Command(Command::TogglePause));
        }
        if i.key_pressed(egui::Key::Escape) && app.now_playing {
            actions.push(Action::NowPlaying(false));
        }
        if i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft) {
            actions.push(Action::Back);
        }
    });
}

// ---------- small pieces ----------

fn font(weight: Weight, size: f32) -> FontId {
    weight.font_id(size)
}

fn label(
    ui: &mut Ui,
    text: impl Into<String>,
    size: f32,
    weight: Weight,
    color: Color32,
) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(text.into())
                .font(font(weight, size))
                .color(color),
        )
        .truncate()
        .selectable(false),
    )
}

/// A clickable line of runs; linked runs open their page.
fn runs_line(ui: &mut Ui, runs: &[Run], size: f32, p: &Palette, actions: &mut Vec<Action>) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
            for run in runs {
                let text = RichText::new(&run.text)
                    .font(font(Weight::Regular, size))
                    .color(p.secondary);
                match &run.target {
                    Some(target) => {
                        let response = ui.add(
                            egui::Label::new(text)
                                .sense(Sense::click())
                                .selectable(false),
                        );
                        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.hovered() {
                            let r = response.rect;
                            ui.painter().line_segment(
                                [pos2(r.left(), r.bottom()), pos2(r.right(), r.bottom())],
                                Stroke::new(1.0, p.secondary),
                            );
                        }
                        if response.clicked() {
                            actions.push(Action::Activate(target.clone()));
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(text).selectable(false));
                    }
                }
            }
        });
    });
}

fn runs_text(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// "Artist, Artist • Album", with each name linked to its page.
fn track_line(track: &Track) -> Vec<Run> {
    let mut line = track.artists.clone();
    if let Some(album) = &track.album {
        if !line.is_empty() {
            line.push(Run {
                text: " • ".into(),
                target: None,
            });
        }
        line.push(album.clone());
    }
    line
}

/// Names a custom-drawn control for screen readers (AccessKit) and, in E2E
/// runs, for the driver that clicks controls by name.
fn named(response: egui::Response, label: &str) -> egui::Response {
    named_as(response, egui::WidgetType::Button, label)
}

fn named_as(response: egui::Response, kind: egui::WidgetType, label: &str) -> egui::Response {
    let text = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(kind, true, &text));
    #[cfg(feature = "e2e")]
    crate::e2e::register(&response.ctx, label, response.interact_rect);
    response
}

/// The pointer has rested on `response` for a moment: a likely click.
fn resting(ui: &Ui, response: &egui::Response) -> bool {
    if !response.hovered() {
        return false;
    }
    let still = ui.input(|i| i.pointer.time_since_last_movement());
    if still < 0.4 {
        // egui draws nothing while the pointer is still; come back to check.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f32(0.45 - still));
        return false;
    }
    true
}

/// A round icon button with a hover disc.
fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    size: f32,
    color: Color32,
    p: &Palette,
    tip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size + 16.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .circle_filled(rect.center(), rect.width() / 2.0, p.surface_hover);
    }
    icon.image(color, size)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    named(response, tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// A pill button, filled with the accent when `primary`.
fn pill(ui: &mut Ui, text: &str, icon: Option<Icon>, primary: bool, p: &Palette) -> egui::Response {
    let (fill, fg) = if primary {
        (p.accent, p.on_accent)
    } else {
        (p.surface, p.text)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(Weight::Medium, 14.0), fg);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 32.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() {
        if primary {
            p.accent_hover
        } else {
            p.surface_hover
        }
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(18), fill);
    let mut x = rect.left() + 16.0;
    if let Some(icon) = icon {
        icon.image(fg, 18.0).paint_at(
            ui,
            Rect::from_min_size(pos2(x, rect.center().y - 9.0), Vec2::splat(18.0)),
        );
        x += icon_w;
    }
    ui.painter()
        .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    named(response, text).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Cover art with a placeholder of the same shape while it loads.
fn cover(ui: &mut Ui, rect: Rect, url: Option<&str>, round: bool, radius: u8, p: &Palette) {
    let corner = if round {
        CornerRadius::same((rect.width() / 2.0).min(255.0) as u8)
    } else {
        CornerRadius::same(radius)
    };
    ui.painter().rect_filled(rect, corner, p.surface);
    match url {
        Some(url) => {
            egui::Image::new(url)
                .corner_radius(corner)
                .show_loading_spinner(false)
                .paint_at(ui, rect);
        }
        None => {
            let s = (rect.width() * 0.35).clamp(14.0, 64.0);
            Icon::Music
                .image(p.dim, s)
                .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(s)));
        }
    }
}

fn play_disc(ui: &mut Ui, center: egui::Pos2, radius: f32, p: &Palette, hovered: bool) {
    let fill = if hovered { p.accent_hover } else { p.accent };
    ui.painter().circle_filled(center, radius, fill);
    let s = radius * 0.95;
    Icon::Play.image(p.on_accent, s).paint_at(
        ui,
        Rect::from_center_size(center + vec2(radius * 0.06, 0.0), Vec2::splat(s)),
    );
}

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

// ---------- chrome ----------

fn sidebar(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        Icon::Music
            .image(p.accent, 24.0)
            .paint_at(ui, Rect::from_min_size(ui.cursor().min, Vec2::splat(24.0)));
        ui.add_space(32.0);
        label(ui, "Music", 20.0, Weight::Bold, p.text);
    });
    ui.add_space(20.0);
    let current = match &app.view {
        View::Home => 0,
        View::Explore => 1,
        View::Library(_) => 2,
        View::Page(_) => 3,
    };
    for (i, (icon, text, view)) in [
        (Icon::Home, "Home", View::Home),
        (Icon::Explore, "Explore", View::Explore),
        (
            Icon::Library,
            "Library",
            View::Library(LibraryTab::Playlists),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let selected = current == i && !app.now_playing;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
        let fill = if selected {
            p.surface_active
        } else if response.hovered() {
            p.surface_hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        icon.image(p.text, 22.0).paint_at(
            ui,
            Rect::from_min_size(
                pos2(rect.left() + 16.0, rect.center().y - 11.0),
                Vec2::splat(22.0),
            ),
        );
        ui.painter().text(
            pos2(rect.left() + 56.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font(Weight::Medium, 15.0),
            p.text,
        );
        if named(response, text)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            let view = match (&app.view, view) {
                // Library remembers which tab was open.
                (View::Library(tab), View::Library(_)) => View::Library(*tab),
                (_, view) => view,
            };
            actions.push(Action::Open(view));
        }
    }
    ui.add_space(12.0);
    ui.painter().hline(
        ui.max_rect().x_range(),
        ui.cursor().top(),
        Stroke::new(1.0, p.outline),
    );
    ui.add_space(12.0);
    // The account's playlists, as YouTube Music lists them in its sidebar.
    let Some(page) = app
        .page_state(&LibraryTab::Playlists.target())
        .and_then(|s| s.page.as_ref())
    else {
        return;
    };
    let playlists: Vec<&Item> = page
        .shelves
        .iter()
        .flat_map(|s| s.items.iter())
        .filter(|i| i.target.is_some())
        .collect();
    ScrollArea::vertical()
        .id_salt("sidebar-playlists")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for item in playlists {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
                let open = matches!(&app.view, View::Page(t) if Some(t) == item.target.as_ref());
                let fill = if open {
                    p.surface_active
                } else if response.hovered() {
                    p.surface_hover
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
                let text_rect = rect.shrink2(vec2(12.0, 6.0));
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                label(&mut child, &item.title, 14.0, Weight::Medium, p.text);
                label(
                    &mut child,
                    runs_text(&item.subtitle),
                    12.5,
                    Weight::Regular,
                    p.secondary,
                );
                if named(response, &item.title)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    && let Some(target) = &item.target
                {
                    actions.push(Action::Activate(target.clone()));
                }
            }
        });
}

fn top_bar(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    ui.horizontal_centered(|ui| {
        if !app.history.is_empty() || app.now_playing {
            if icon_button(ui, Icon::Back, 20.0, p.text, p, "Back").clicked() {
                actions.push(if app.now_playing {
                    Action::NowPlaying(false)
                } else {
                    Action::Back
                });
            }
        } else {
            ui.add_space(36.0);
        }
        ui.add_space(12.0);
        search_box(app, ui, p, actions);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            account(app, ui, p, actions);
            ui.add_space(4.0);
            settings(app, ui, p, actions);
        });
    });
}

fn search_box(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let width = (ui.available_width() - 120.0).clamp(240.0, 520.0);
    let frame = Frame::new()
        .fill(p.surface)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8));
    let inner = frame.show(ui, |ui| {
        ui.set_width(width - 24.0);
        ui.horizontal(|ui| {
            Icon::Search.image(p.secondary, 18.0).paint_at(
                ui,
                Rect::from_min_size(ui.cursor().min + vec2(0.0, 1.0), Vec2::splat(18.0)),
            );
            ui.add_space(26.0);
            let edit = egui::TextEdit::singleline(&mut app.search)
                .hint_text(RichText::new("Search songs, albums, artists, playlists").color(p.dim))
                .font(font(Weight::Regular, 15.0))
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY);
            ui.add(edit)
        })
        .inner
    });
    let response = inner.inner;
    #[cfg(feature = "e2e")]
    crate::e2e::register(&response.ctx, "Search", response.interact_rect);
    if response.changed() {
        app.search_changed();
    }
    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        actions.push(Action::Search(app.search.clone()));
    }
    // Suggestions below the field while it has focus.
    if response.has_focus() && !app.suggestions.is_empty() && !app.search.trim().is_empty() {
        let rect = inner.response.rect;
        egui::Area::new(Id::new("search-suggestions"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom() + vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.outline))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_width(rect.width() - 12.0);
                        for suggestion in app.suggestions.iter().take(8) {
                            let (row, r) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 36.0),
                                Sense::click(),
                            );
                            if r.hovered() {
                                ui.painter().rect_filled(
                                    row,
                                    CornerRadius::same(6),
                                    p.surface_hover,
                                );
                            }
                            Icon::Search.image(p.dim, 16.0).paint_at(
                                ui,
                                Rect::from_min_size(
                                    pos2(row.left() + 10.0, row.center().y - 8.0),
                                    Vec2::splat(16.0),
                                ),
                            );
                            ui.painter().text(
                                pos2(row.left() + 36.0, row.center().y),
                                Align2::LEFT_CENTER,
                                suggestion,
                                font(Weight::Regular, 14.0),
                                p.text,
                            );
                            // Pressed, not clicked: the field loses focus on press, which hides the list.
                            if r.is_pointer_button_down_on() {
                                actions.push(Action::Search(suggestion.clone()));
                            }
                        }
                    });
            });
    }
}

fn account(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    match &app.account {
        Account::SignedIn {
            name,
            photo,
            source,
        } => {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            match photo {
                Some(url) => cover(ui, rect, Some(url), true, 0, p),
                None => {
                    ui.painter().circle_filled(rect.center(), 16.0, p.accent);
                    let initial = name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string();
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        initial,
                        font(Weight::SemiBold, 15.0),
                        p.on_accent,
                    );
                }
            }
            response.on_hover_text(format!("{name}\nSigned in through {source}"));
        }
        Account::Checking => {
            ui.add(egui::Spinner::new().size(18.0).color(p.secondary));
        }
        Account::SignedOut { reason } | Account::Unverified { reason } => {
            if pill(ui, "Reconnect", Some(Icon::Refresh), true, p)
                .on_hover_text(reason.as_str())
                .clicked()
            {
                actions.push(Action::Command(Command::Reconnect));
            }
            let text = if matches!(app.account, Account::SignedOut { .. }) {
                "Signed out of YouTube Music"
            } else {
                "Offline"
            };
            label(ui, text, 13.0, Weight::Medium, p.warning).on_hover_text(reason.as_str());
        }
    }
}

fn settings(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let response = icon_button(ui, Icon::Settings, 20.0, p.secondary, p, "Settings");
    egui::Popup::menu(&response).width(320.0).show(|ui| {
        ui.set_min_width(300.0);
        label(ui, "Audio quality", 12.0, Weight::SemiBold, p.secondary);
        let format = app
            .playback
            .format
            .clone()
            .unwrap_or_else(|| "Nothing playing".into());
        label(ui, format, 14.0, Weight::Regular, p.text);
        ui.add_space(8.0);
        let mut autoplay = app.playback.autoplay;
        if ui
            .checkbox(&mut autoplay, "Autoplay similar songs when the queue ends")
            .changed()
        {
            actions.push(Action::Command(Command::Autoplay(autoplay)));
        }
        ui.add_space(8.0);
        label(ui, "Account", 12.0, Weight::SemiBold, p.secondary);
        let status = match &app.account {
            Account::SignedIn { name, source, .. } => format!("{name} · {source}"),
            Account::Checking => "Checking…".into(),
            Account::SignedOut { reason } | Account::Unverified { reason } => reason.clone(),
        };
        ui.add(
            egui::Label::new(
                RichText::new(status)
                    .font(font(Weight::Regular, 13.0))
                    .color(p.text),
            )
            .wrap(),
        );
        if ui.button("Reconnect to the browser's session").clicked() {
            actions.push(Action::Command(Command::Reconnect));
        }
        // Each signed-in browser profile can be a different Google account.
        if app.profiles.len() > 1 {
            ui.add_space(8.0);
            label(
                ui,
                "Use the YouTube account signed in to",
                12.0,
                Weight::SemiBold,
                p.secondary,
            );
            for profile in &app.profiles {
                let current = app.profile.as_deref() == Some(profile.id.as_str());
                if ui.radio(current, profile.label.as_str()).clicked() && !current {
                    actions.push(Action::Command(Command::UseProfile(profile.id.clone())));
                }
            }
        }
    });
}

fn errors(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    if app.errors.is_empty() {
        return;
    }
    let area = ui.max_rect();
    let width = (area.width() - 48.0).min(640.0);
    let mut y = area.bottom() - 16.0;
    for (i, error) in app.errors.iter().enumerate().rev() {
        let galley = ui.painter().layout(
            error.clone(),
            font(Weight::Regular, 14.0),
            p.text,
            width - 120.0,
        );
        let height = galley.size().y + 24.0;
        let rect = Rect::from_min_size(
            pos2(area.center().x - width / 2.0, y - height),
            vec2(width, height),
        );
        y -= height + 8.0;
        ui.painter().rect(
            rect,
            CornerRadius::same(10),
            p.panel,
            Stroke::new(1.0, p.danger),
            egui::StrokeKind::Inside,
        );
        Icon::Alert.image(p.danger, 18.0).paint_at(
            ui,
            Rect::from_min_size(rect.left_top() + vec2(14.0, 12.0), Vec2::splat(18.0)),
        );
        ui.painter()
            .galley(rect.left_top() + vec2(44.0, 12.0), galley, p.text);
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(rect.right() - 80.0, rect.top() + 2.0),
                    rect.right_bottom(),
                ))
                .layout(Layout::left_to_right(Align::Min)),
        );
        if icon_button(&mut buttons, Icon::Copy, 16.0, p.secondary, p, "Copy").clicked() {
            actions.push(Action::Copy(error.clone()));
        }
        if icon_button(&mut buttons, Icon::Close, 16.0, p.secondary, p, "Dismiss").clicked() {
            actions.push(Action::DismissError(i));
        }
    }
}

// ---------- pages ----------

fn content(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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
    });
}

fn chip(ui: &mut Ui, text: &str, selected: bool, p: &Palette) -> egui::Response {
    let (fill, fg) = if selected {
        (p.text, p.window)
    } else {
        (p.surface, p.text)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(Weight::Medium, 14.0), fg);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 24.0, 32.0), Sense::click());
    let fill = if response.hovered() && !selected {
        p.surface_hover
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    named(response, text).on_hover_cursor(egui::CursorIcon::PointingHand)
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

fn skeleton_shelf(ui: &mut Ui, p: &Palette) {
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
            });
        });
    });
}

fn shelf_view(
    ui: &mut Ui,
    shelf: &Shelf,
    id: (&str, usize),
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let scroll_id = Id::new(("carousel", id.0, id.1));
    // The id the scroll area stores its offset under, for the arrows.
    let scroll_state = ui.make_persistent_id(scroll_id);
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
                    if icon_button(ui, Icon::ChevronRight, 18.0, p.text, p, "Next").clicked() {
                        nudge(ui, scroll_state, scroll_id, 1.0);
                    }
                    if icon_button(ui, Icon::ChevronLeft, 18.0, p.text, p, "Previous").clicked() {
                        nudge(ui, scroll_state, scroll_id, -1.0);
                    }
                    ui.add_space(4.0);
                }
                if let Some(more) = &shelf.more
                    && pill(ui, "More", None, false, p).clicked()
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

/// Scrolls a carousel by most of its visible width.
fn nudge(ui: &Ui, id: Id, scroll_id: Id, direction: f32) {
    let width: f32 = ui
        .data(|d| d.get_temp(scroll_id.with("width")))
        .unwrap_or(800.0);
    if let Some(mut state) = egui::scroll_area::State::load(ui.ctx(), id) {
        state.offset.x = (state.offset.x + direction * (width - CARD).max(CARD)).max(0.0);
        state.store(ui.ctx(), id);
    }
}

fn remember_width(ui: &Ui, scroll_id: Id, width: f32) {
    ui.data_mut(|d| d.insert_temp(scroll_id.with("width"), width));
}

fn card(ui: &mut Ui, item: &Item, shelf: &Shelf, p: &Palette, actions: &mut Vec<Action>) {
    let round = item.kind == ItemKind::Artist;
    let (rect, response) = ui.allocate_exact_size(vec2(CARD, CARD + 58.0), Sense::click());
    let art = Rect::from_min_size(rect.min, Vec2::splat(CARD));
    cover(ui, art, item.thumbnail.as_deref(), round, 6, p);
    let hovered = response.hovered();
    let playable = item.play.is_some() || item.track.is_some();
    let mut play_hit = false;
    if hovered && !round {
        ui.painter()
            .rect_filled(art, CornerRadius::same(6), p.overlay.gamma_multiply(0.45));
    }
    if hovered && playable {
        let center = art.right_bottom() - vec2(30.0, 30.0);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        play_hit = pointer.is_some_and(|pos| pos.distance(center) < 20.0);
        play_disc(ui, center, 20.0, p, play_hit);
    }
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
    if response.clicked() {
        if play_hit {
            play_item(item, shelf, actions)
        } else {
            activate(item, shelf, actions)
        }
    }
}

fn row(
    ui: &mut Ui,
    item: &Item,
    shelf: &Shelf,
    wide: bool,
    p: &Palette,
    actions: &mut Vec<Action>,
) {
    let height = 56.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), p.surface_hover);
    }
    let thumb = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 20.0),
        Vec2::splat(40.0),
    );
    let show_index = item.index.is_some() && (item.thumbnail.is_none() || shelf_is_album(shelf));
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
    let right = rect.right() - if duration.is_some() { 72.0 } else { 16.0 };
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
    if let Some(duration) = duration {
        ui.painter().text(
            pos2(rect.right() - 16.0, rect.center().y),
            Align2::RIGHT_CENTER,
            duration,
            font(Weight::Regular, 14.0),
            p.secondary,
        );
    }
    // Links inside the row take their own clicks; the rest of the row plays/opens.
    let response = named(response, &item.title);
    if let (true, Some(track)) = (resting(ui, &response), &item.track) {
        actions.push(Action::Prepare(track.video_id.clone()));
    }
    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
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
                cover(
                    ui,
                    rect,
                    item.thumbnail.as_deref(),
                    item.kind == ItemKind::Artist,
                    6,
                    p,
                );
                if named(response, &item.title)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
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
                        activate(item, shelf, actions);
                    }
                    runs_line(ui, &item.subtitle, 14.0, p, actions);
                    ui.add_space(10.0);
                    if (item.play.is_some() || item.track.is_some())
                        && pill(ui, "Play", Some(Icon::Play), true, p).clicked()
                    {
                        play_item(item, shelf, actions);
                    }
                });
            });
        });
}

// ---------- player ----------

fn player_bar(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let pb = &app.playback;
    let full = ui.max_rect();
    // Progress along the top edge; it thickens on hover and seeks on click or drag.
    let bar = Rect::from_min_size(full.min, vec2(full.width(), 12.0));
    let response = named_as(
        ui.interact(bar, Id::new("seek"), Sense::click_and_drag()),
        egui::WidgetType::Slider,
        "Seek",
    );
    let active = response.hovered() || response.dragged();
    let duration = pb.duration.max(0.0);
    let pointer_fraction = response
        .interact_pointer_pos()
        .map(|pos| ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0));
    let shown = match (response.dragged(), pointer_fraction) {
        (true, Some(f)) => f64::from(f) * duration,
        _ => pb.position,
    };
    if (response.drag_stopped() || response.clicked())
        && duration > 0.0
        && let Some(f) = pointer_fraction
    {
        actions.push(Action::Command(Command::Seek(f64::from(f) * duration)));
    }
    let thickness = if active { 4.0 } else { 2.0 };
    let track_rect = Rect::from_min_size(full.min, vec2(full.width(), thickness));
    ui.painter().rect_filled(track_rect, 0.0, p.surface_active);
    let fraction = if duration > 0.0 {
        (shown / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    ui.painter().rect_filled(
        Rect::from_min_size(full.min, vec2(full.width() * fraction, thickness)),
        0.0,
        p.accent,
    );
    if active {
        ui.painter().circle_filled(
            pos2(
                full.left() + full.width() * fraction,
                full.top() + thickness / 2.0,
            ),
            6.0,
            p.accent,
        );
        if let Some(pos) = response.hover_pos() {
            let at = f64::from(((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0)) * duration;
            response.clone().on_hover_text_at_pointer(format_time(at));
        }
    }

    let inner = full.shrink2(vec2(16.0, 0.0)).with_min_y(full.top() + 4.0);
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut ui;
    // Transport.
    if icon_button(ui, Icon::SkipBack, 22.0, p.text, p, "Previous").clicked() {
        actions.push(Action::Command(Command::Previous));
    }
    let (rect, play) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::click());
    if play.hovered() {
        ui.painter()
            .circle_filled(rect.center(), 24.0, p.surface_hover);
    }
    if pb.loading {
        egui::Spinner::new()
            .size(28.0)
            .color(p.text)
            .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(28.0)));
    } else {
        let icon = if pb.playing { Icon::Pause } else { Icon::Play };
        icon.image(p.text, 30.0)
            .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(30.0)));
    }
    if named(play, if pb.playing { "Pause" } else { "Play" })
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(if pb.playing { "Pause" } else { "Play" })
        .clicked()
    {
        actions.push(Action::Command(Command::TogglePause));
    }
    if icon_button(ui, Icon::SkipForward, 22.0, p.text, p, "Next").clicked() {
        actions.push(Action::Command(Command::Next));
    }
    ui.add_space(8.0);
    label(
        ui,
        format!("{} / {}", format_time(shown), format_time(duration)),
        13.0,
        Weight::Regular,
        p.secondary,
    );

    // Right side first, so the middle gets what is left.
    let right_width = 300.0;
    let middle_width = (ui.available_width() - right_width).max(160.0);
    let (middle, _) = ui.allocate_exact_size(vec2(middle_width, PLAYER - 8.0), Sense::hover());
    let current = pb.index.and_then(|i| app.queue.get(i));
    if let Some(track) = current {
        let mut mid = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(middle.shrink2(vec2(24.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let (art, art_response) = mid.allocate_exact_size(Vec2::splat(48.0), Sense::click());
        cover(&mut mid, art, track.thumbnail.as_deref(), false, 4, p);
        if named(art_response, "Cover")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            actions.push(Action::NowPlaying(!app.now_playing));
        }
        mid.add_space(12.0);
        mid.vertical(|ui| {
            ui.add_space(4.0);
            let title = ui.add(
                egui::Label::new(
                    RichText::new(&track.title)
                        .font(font(Weight::SemiBold, 15.0))
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
                actions.push(Action::NowPlaying(!app.now_playing));
            }
            runs_line(ui, &track_line(track), 13.5, p, actions);
        });
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let chevron = if app.now_playing {
            Icon::ChevronDown
        } else {
            Icon::ChevronUp
        };
        if icon_button(
            ui,
            chevron,
            22.0,
            p.text,
            p,
            if app.now_playing {
                "Close player"
            } else {
                "Open player"
            },
        )
        .clicked()
        {
            actions.push(Action::NowPlaying(!app.now_playing));
        }
        let shuffle_color = if pb.shuffle { p.accent } else { p.secondary };
        if icon_button(
            ui,
            Icon::Shuffle,
            20.0,
            shuffle_color,
            p,
            if pb.shuffle {
                "Shuffle on"
            } else {
                "Shuffle off"
            },
        )
        .clicked()
        {
            actions.push(Action::Command(Command::ToggleShuffle));
        }
        let (icon, color, tip) = match pb.repeat {
            Repeat::Off => (Icon::Repeat, p.secondary, "Repeat off"),
            Repeat::All => (Icon::Repeat, p.accent, "Repeat all"),
            Repeat::One => (Icon::RepeatOne, p.accent, "Repeat one"),
        };
        if icon_button(ui, icon, 20.0, color, p, tip).clicked() {
            actions.push(Action::Command(Command::CycleRepeat));
        }
        let mut volume = pb.volume as f32;
        let slider = ui.add_sized(
            vec2(96.0, 20.0),
            egui::Slider::new(&mut volume, 0.0..=100.0)
                .show_value(false)
                .trailing_fill(true),
        );
        if slider.changed() {
            actions.push(Action::Command(Command::Volume(f64::from(volume))));
        }
        let icon = if pb.volume <= 0.0 {
            Icon::Mute
        } else {
            Icon::Volume
        };
        if icon_button(ui, icon, 20.0, p.secondary, p, "Mute").clicked() {
            actions.push(Action::Command(Command::Volume(if pb.volume > 0.0 {
                0.0
            } else {
                70.0
            })));
        }
    });
}

fn now_playing(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let area = ui.max_rect().shrink2(vec2(40.0, 24.0));
    let side = 440.0_f32.min(area.width() * 0.45);
    let art_area = Rect::from_min_max(area.min, pos2(area.right() - side - 40.0, area.bottom()));
    let side_area = Rect::from_min_max(pos2(area.right() - side, area.top()), area.max);
    let current = app.playback.index.and_then(|i| app.queue.get(i));

    // The cover, as large as fits.
    let size = art_area.width().min(art_area.height() - 72.0).max(120.0);
    let art = Rect::from_center_size(
        pos2(art_area.center().x, art_area.top() + size / 2.0 + 8.0),
        Vec2::splat(size),
    );
    cover(
        ui,
        art,
        current.and_then(|t| t.thumbnail.as_deref()),
        false,
        8,
        p,
    );
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
        if let Some(format) = &app.playback.format {
            label(&mut below, format, 12.0, Weight::Regular, p.dim);
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
                NowPlayingTab::Lyrics => app.playback.lyrics.is_some(),
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
        NowPlayingTab::Lyrics => lyrics(app, ui, p, actions),
        NowPlayingTab::Related => related(app, ui, p, actions),
    }
}

fn up_next(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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
    });
    ui.add_space(6.0);
    let current = app.playback.index;
    ScrollArea::vertical()
        .id_salt("up-next")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, track) in app.queue.iter().enumerate() {
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
                let is_current = current == Some(i);
                if is_current {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), p.surface_active);
                    if app.now_playing
                        && ui.ctx().memory(|m| {
                            m.data.get_temp::<usize>(Id::new("up-next-scrolled")) != Some(i)
                        })
                    {
                        ui.scroll_to_rect(rect, Some(Align::Center));
                        ui.ctx()
                            .memory_mut(|m| m.data.insert_temp(Id::new("up-next-scrolled"), i));
                    }
                } else if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), p.surface_hover);
                }
                let thumb = Rect::from_min_size(
                    pos2(rect.left() + 8.0, rect.center().y - 20.0),
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
                if let Some(d) = track.duration {
                    ui.painter().text(
                        pos2(rect.right() - 10.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format_time(f64::from(d)),
                        font(Weight::Regular, 13.0),
                        p.secondary,
                    );
                }
                if named(response, &track.title)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    && !is_current
                {
                    actions.push(Action::Command(Command::JumpTo(i)));
                }
            }
        });
}

fn lyrics(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let Some(id) = &app.playback.lyrics else {
        label(
            ui,
            "Lyrics aren't available for this song.",
            15.0,
            Weight::Regular,
            p.secondary,
        );
        return;
    };
    match app.lyrics.get(id) {
        None => {
            actions.push(Action::Lyrics(id.clone()));
            ui.add(egui::Spinner::new().size(20.0).color(p.secondary));
        }
        Some(Err(error)) => {
            ui.horizontal(|ui| {
                label(
                    ui,
                    format!("Couldn't load the lyrics. {error}"),
                    14.0,
                    Weight::Regular,
                    p.secondary,
                );
                if ui.link("Retry").clicked() {
                    actions.push(Action::RetryLyrics(id.clone()));
                }
            });
        }
        Some(Ok(None)) => {
            label(
                ui,
                "Lyrics aren't available for this song.",
                15.0,
                Weight::Regular,
                p.secondary,
            );
        }
        Some(Ok(Some(lyrics))) => {
            ScrollArea::vertical()
                .id_salt(("lyrics", id))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&lyrics.text)
                                .font(font(Weight::Medium, 17.0))
                                .color(p.text),
                        )
                        .wrap(),
                    );
                    if let Some(source) = &lyrics.source {
                        ui.add_space(16.0);
                        label(ui, source, 12.0, Weight::Regular, p.dim);
                    }
                });
        }
    }
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
