//! The interface, laid out like YouTube Music: navigation on the left, the
//! search bar on top, the page in the middle and the player bar below.
//! Views read [`App`] and push [`Action`]s.

mod account;
mod chrome;
mod equalizer;
mod lyrics;
pub mod mini;
pub(crate) mod motion;
mod now_playing;
mod pages;
mod player;
mod ridge;
mod settings;
mod shelves;
mod sleep;
pub(crate) mod stage;
mod widgets;

use crate::app::{Action, App, WindowKind};
use crate::backend::Command;
use chrome::{errors, sidebar, top_bar};
use egui::{Frame, Margin, Ui};
use now_playing::now_playing;
use pages::content;
use player::player_bar;

const SIDEBAR: f32 = 232.0;
const TOPBAR: f32 = 64.0;
const PLAYER: f32 = 76.0;
const CARD: f32 = 176.0;
const GAP: f32 = 16.0;

pub fn draw(app: &mut App, ui: &mut Ui, actions: &mut Vec<Action>) {
    let p = app.palette.clone();
    keyboard(app, ui, actions);
    account::publish(app, ui);
    if app.stage.open && !app.queue.is_empty() {
        stage::stage(app, ui, actions);
        return;
    }
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
            motion::clear_origin(ui.ctx(), "header");
            // Now Playing takes colour from the cover: never theme-painted.
            let ctx = ui.ctx().clone();
            crate::derived::without_paint(&ctx, || now_playing(app, ui, &p, actions));
        } else {
            motion::clear_origin(ui.ctx(), "now-playing");
            content(app, ui, &p, actions);
        }
        errors(app, ui, &p, actions);
    });
    account::dialogs(app, ui, &p, actions);
    if app.equalizer_open {
        equalizer::equalizer(app, ui.ctx(), &p, actions);
    }
}

fn keyboard(app: &App, ui: &Ui, actions: &mut Vec<Action>) {
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    ui.input(|i| {
        if !typing && i.key_pressed(egui::Key::Space) && !app.queue.is_empty() {
            actions.push(Action::Command(Command::TogglePause));
        }
        if i.key_pressed(egui::Key::Escape) && app.now_playing && !app.stage.open {
            actions.push(Action::NowPlaying(false));
        }
        if i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft) {
            actions.push(Action::Back);
        }
        if i.modifiers.command && i.key_pressed(egui::Key::Q) {
            actions.push(Action::Quit);
        }
        if i.modifiers.command && i.key_pressed(egui::Key::M) {
            actions.push(Action::MiniPlayer(app.window == WindowKind::Main));
        }
        stage::keys(app, i, typing, actions);
    });
}
