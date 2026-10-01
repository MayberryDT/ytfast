use super::widgets::{font, icon_button, label, named, named_as};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::Account;
use crate::theme::Palette;
use egui::{RichText, Ui};
use fastframe_fonts::Weight;

pub(super) fn settings(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
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
        let mut normalize = app.playback.normalize;
        if named_as(
            ui.checkbox(&mut normalize, "Even out loudness between songs"),
            egui::WidgetType::Checkbox,
            "Even out loudness between songs",
        )
        .changed()
        {
            actions.push(Action::Command(Command::Normalize(normalize)));
        }
        if let (true, Some(gain)) = (app.playback.normalize, app.playback.gain) {
            label(
                ui,
                format!("This song plays at {gain:+.1} dB"),
                12.0,
                Weight::Regular,
                p.dim,
            );
        }
        let equalizer = &app.playback.equalizer;
        let status = if equalizer.enabled {
            equalizer.preset.label()
        } else {
            "Off"
        };
        if named(ui.button(format!("Equalizer · {status}")), "Open equalizer").clicked() {
            actions.push(Action::ShowEqualizer(true));
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
