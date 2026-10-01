use super::settings::settings;
use super::widgets::{cover, font, icon_button, label, named, pill, runs_text};
use crate::app::{Action, App, LibraryTab, View};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{Account, Item};
use crate::theme::Palette;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, RichText, ScrollArea,
    Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use fastframe_fonts::Weight;

pub(super) fn sidebar(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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

pub(super) fn top_bar(app: &mut App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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

pub(super) fn account(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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

pub(super) fn errors(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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
