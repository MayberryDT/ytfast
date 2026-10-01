//! The sleep timer in the player bar: a moon with a small menu, and the
//! time left beside it while a timer is set.

use std::time::{Duration, Instant};

use super::widgets::{icon_button, label, named};
use crate::app::{Action, App};
use crate::backend::Command;
use crate::icons::Icon;
use crate::model::{Sleep, format_time};
use crate::theme::Palette;
use egui::Ui;
use fastframe_fonts::Weight;

const CHOICES: [(&str, Option<Sleep>); 6] = [
    ("15 minutes", Some(Sleep::Minutes(15))),
    ("30 minutes", Some(Sleep::Minutes(30))),
    ("45 minutes", Some(Sleep::Minutes(45))),
    ("1 hour", Some(Sleep::Minutes(60))),
    ("End of song", Some(Sleep::EndOfSong)),
    ("Off", None),
];

/// Drawn in a right-to-left layout: the moon, then the time left to its left.
pub(super) fn sleep_timer(app: &App, ui: &mut Ui, p: &Palette, actions: &mut Vec<Action>) {
    let timer = app.playback.sleep;
    let color = if timer.is_some() {
        p.accent
    } else {
        p.secondary
    };
    let response = icon_button(ui, Icon::Moon, 20.0, color, p, "Sleep timer");
    if let Some(timer) = timer {
        let left = match (timer.choice, timer.deadline) {
            (Sleep::Minutes(_), Some(deadline)) => {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                format_time(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_secs_f64()
                        .ceil(),
                )
            }
            _ => "End of song".to_owned(),
        };
        label(ui, left, 12.5, Weight::Medium, p.accent);
    }
    egui::Popup::menu(&response).width(180.0).show(|ui| {
        ui.set_min_width(160.0);
        label(ui, "Sleep timer", 12.0, Weight::SemiBold, p.secondary);
        ui.add_space(4.0);
        for (text, choice) in CHOICES {
            let selected = timer.map(|t| t.choice) == choice;
            if named(ui.selectable_label(selected, text), text).clicked() {
                actions.push(Action::Command(Command::SleepTimer(choice)));
            }
        }
    });
}
