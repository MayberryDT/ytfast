//! Motion: springs, covers that fly between places, and changes that roll.
//!
//! Everything here is driven by the frame clock and stored in egui's
//! temporary memory, so it costs nothing when still and never delays input:
//! a new target simply redirects whatever is in flight.

use egui::{Context, CornerRadius, Id, Rect, Ui, vec2};

/// Soft overshoot: a cover lands with a small settle, not a bounce.
const DAMPING: f32 = 0.74;
/// Angular frequency of the landing spring (rad/s).
const OMEGA: f32 = 15.0;
/// A launched cover that no place claims within this time is dropped.
const UNCLAIMED: f64 = 0.9;
/// How long a flight lasts before the destination draws itself again.
const FLIGHT: f64 = 0.62;

/// Progress of an underdamped spring released at `t = 0` from 0 towards 1.
pub fn settle(t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    let damped = OMEGA * (1.0 - DAMPING * DAMPING).sqrt();
    let decay = (-DAMPING * OMEGA * t).exp();
    1.0 - decay * ((damped * t).cos() + DAMPING * OMEGA / damped * (damped * t).sin())
}

/// Ease-out for things that should arrive without overshoot (fades, rolls).
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Default)]
struct Spring {
    value: f32,
    velocity: f32,
}

/// A value that follows `target` like a mass on a spring. `stiffness` around
/// 200–400 feels physical; the damping ratio is fixed just under critical.
pub fn spring(ctx: &Context, id: Id, target: f32, stiffness: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 1.0 / 20.0);
    let mut s = ctx.data(|d| d.get_temp::<Spring>(id)).unwrap_or(Spring {
        value: target,
        velocity: 0.0,
    });
    let damping = 2.0 * 0.86 * stiffness.sqrt();
    // A few small steps keep stiff springs stable on slow frames.
    let steps = 4;
    let h = dt / steps as f32;
    for _ in 0..steps {
        let force = stiffness * (target - s.value) - damping * s.velocity;
        s.velocity += force * h;
        s.value += s.velocity * h;
    }
    if (s.value - target).abs() < 1e-3 && s.velocity.abs() < 1e-3 {
        s = Spring {
            value: target,
            velocity: 0.0,
        };
    } else {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, s));
    s.value
}

/// 0 → 1 while `on`, with weight: for hover lifts and presses.
pub fn lift(ui: &Ui, id: Id, on: bool) -> f32 {
    spring(ui.ctx(), id.with("lift"), if on { 1.0 } else { 0.0 }, 320.0)
}

#[derive(Clone)]
struct Flight {
    url: String,
    from: Rect,
    from_radius: f32,
    launched: f64,
    /// The place this cover is going to, whatever image it shows there; with
    /// none, any place showing the same image takes it.
    to: Option<Id>,
    /// The place that took the cover and when: only it draws the flight.
    claim: Option<(Id, f64)>,
}

/// The page header's cover: where a card's cover goes when it opens a page.
pub fn header_site() -> Id {
    Id::new("page-header-cover")
}

/// The player bar's cover: where a song's cover goes when it starts.
pub fn player_site() -> Id {
    Id::new("player-cover")
}

/// Now Playing's large cover.
pub fn now_playing_site() -> Id {
    Id::new("now-playing-cover")
}

fn flight_id() -> Id {
    Id::new("ytfast-flying-cover")
}

fn origins_id() -> Id {
    Id::new("ytfast-cover-origins")
}

/// The image a cover URL shows, without its size: the same album art is
/// asked for at different sizes and qualities in different places.
fn cover_identity(url: &str) -> &str {
    if let Some(rest) = url.split("i.ytimg.com/vi/").nth(1) {
        // https://i.ytimg.com/vi/<video id>/<quality>.jpg
        return rest.split('/').next().unwrap_or(rest);
    }
    match url.rfind('=') {
        Some(eq) if url.contains("googleusercontent.com") || url.contains("ggpht.com") => {
            &url[..eq]
        }
        _ => url,
    }
}

fn same_cover(a: &str, b: &str) -> bool {
    cover_identity(a) == cover_identity(b)
}

/// A cover leaves `from` for the place `to` (an album card → the album's
/// header, a song → the player), whatever image that place ends up showing.
pub fn launch_to(ctx: &Context, url: &str, from: Rect, radius: f32, to: Id) {
    send(ctx, url, from, radius, Some(to));
}

fn send(ctx: &Context, url: &str, from: Rect, radius: f32, to: Option<Id>) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        d.insert_temp(
            flight_id(),
            Flight {
                url: url.to_owned(),
                from,
                from_radius: radius,
                launched: now,
                to,
                claim: None,
            },
        )
    });
    ctx.request_repaint();
}

/// Records where a large cover is shown this frame, so leaving its place
/// (Back, closing Now Playing) can send it home with [`launch_from_origin`].
pub fn origin(ctx: &Context, site: &str, url: &str, rect: Rect, radius: f32) {
    ctx.data_mut(|d| {
        let map = d.get_temp_mut_or_default::<Origins>(origins_id());
        map.0.retain(|o| o.0 != site);
        map.0.push((site.to_owned(), url.to_owned(), rect, radius));
    });
}

#[derive(Clone, Default)]
struct Origins(Vec<(String, String, Rect, f32)>);

/// Launches the cover last shown at `site`, if any, towards `to` (or, with
/// `None`, towards whatever place shows the same image).
pub fn launch_from_origin(ctx: &Context, site: &str, to: Option<Id>) {
    let found = ctx.data(|d| {
        d.get_temp::<Origins>(origins_id())
            .and_then(|o| o.0.into_iter().find(|o| o.0 == site))
    });
    if let Some((_, url, rect, radius)) = found {
        send(ctx, &url, rect, radius, to);
    }
}

/// Forgets where a cover was, when the place it was shown is gone.
pub fn clear_origin(ctx: &Context, site: &str) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Origins>(origins_id())
            .0
            .retain(|o| o.0 != site)
    });
}

/// The cover on its way to `site` that hasn't landed yet, if any: a page
/// still loading can offer it a place to land straight away.
pub fn unclaimed_for(ctx: &Context, site: Id) -> Option<String> {
    let flight = ctx.data(|d| d.get_temp::<Flight>(flight_id()))?;
    let now = ctx.input(|i| i.time);
    (flight.claim.is_none() && flight.to == Some(site) && now - flight.launched <= UNCLAIMED)
        .then_some(flight.url)
}

/// The url of the cover landing at `site` right now.
pub fn landing_at(ctx: &Context, site: Id) -> Option<String> {
    let flight = ctx.data(|d| d.get_temp::<Flight>(flight_id()))?;
    (flight.claim.map(|(owner, _)| owner) == Some(site)).then_some(flight.url)
}

/// Called where a cover is about to be drawn at `dest`. When a cover in
/// flight is addressed to this place (or, unaddressed, shows the same image)
/// and this place claims it, the flight is painted above everything and
/// `true` comes back: draw only the empty frame there.
pub fn land(ui: &Ui, site: Id, url: Option<&str>, dest: Rect, radius: f32) -> bool {
    let Some(url) = url else { return false };
    let ctx = ui.ctx();
    let Some(mut flight) = ctx.data(|d| d.get_temp::<Flight>(flight_id())) else {
        return false;
    };
    let for_here = match flight.to {
        Some(to) => to == site,
        None => same_cover(&flight.url, url),
    };
    if !for_here {
        return false;
    }
    let now = ctx.input(|i| i.time);
    let claimed_at = match flight.claim {
        Some((owner, at)) if owner == site => at,
        Some(_) => return false,
        None => {
            if now - flight.launched > UNCLAIMED {
                ctx.data_mut(|d| d.remove::<Flight>(flight_id()));
                return false;
            }
            flight.claim = Some((site, now));
            ctx.data_mut(|d| d.insert_temp(flight_id(), flight.clone()));
            now
        }
    };
    let t = (now - claimed_at) as f32;
    // The place's own image may still be on its way: hold the cover there
    // until it is, rather than leave an empty frame.
    let dest_ready = same_cover(&flight.url, url)
        || matches!(
            egui::Image::new(url.to_owned()).load_for_size(ctx, dest.size()),
            Ok(egui::load::TexturePoll::Ready { .. })
        );
    if f64::from(t) >= FLIGHT && (dest_ready || f64::from(t) >= FLIGHT + 2.0) {
        ctx.data_mut(|d| d.remove::<Flight>(flight_id()));
        return false;
    }
    let k = settle(t);
    let rect = lerp_rect(flight.from, dest, k);
    let radius = flight.from_radius + (radius - flight.from_radius) * k.clamp(0.0, 1.0);
    let corner = CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8);
    // A root Ui on a foreground layer spanning the window: the flight crosses
    // panels, so it can't be clipped to the place that claimed it.
    let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("ytfast-flying-cover"));
    let over = Ui::new(
        ctx.clone(),
        Id::new("ytfast-flying-cover-ui"),
        egui::UiBuilder::new()
            .layer_id(layer)
            .max_rect(ctx.content_rect()),
    );
    // A shadow that is strongest mid-flight: the cover lifts and sets down.
    let height = (k * (1.0 - k)).max(0.0) * 4.0;
    if height > 0.01 {
        let shadow = egui::epaint::Shadow {
            offset: [0, (10.0 * height) as i8],
            blur: (28.0 * height) as u8,
            spread: 0,
            color: over.visuals().window_shadow.color,
        };
        over.painter().add(shadow.as_shape(rect, corner));
    }
    // The cover that left is already decoded; when this place shows another
    // image (or another size of it), that one fades in as the cover lands.
    egui::Image::new(flight.url.clone())
        .corner_radius(corner)
        .show_loading_spinner(false)
        .paint_at(&over, rect);
    if dest_ready && !same_cover(&flight.url, url) {
        let fade = ((t / FLIGHT as f32 - 0.5) / 0.45).clamp(0.0, 1.0);
        if fade > 0.0 {
            egui::Image::new(url.to_owned())
                .corner_radius(corner)
                .tint(egui::Color32::WHITE.gamma_multiply(fade))
                .show_loading_spinner(false)
                .paint_at(&over, rect);
        }
    }
    ctx.request_repaint();
    true
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    let min = a.min + (b.min - a.min) * t;
    let size = a.size() + (b.size() - a.size()) * t;
    Rect::from_min_size(min, vec2(size.x.max(1.0), size.y.max(1.0)))
}

#[derive(Clone)]
struct Change {
    current: String,
    previous: Option<String>,
    at: f64,
}

/// Watches `key` at `id`; for `duration` seconds after it changes, returns
/// the previous key and progress 0 → 1, so the old and new can trade places.
pub fn changed(ctx: &Context, id: Id, key: &str, duration: f64) -> Option<(String, f32)> {
    let now = ctx.input(|i| i.time);
    let mut state = ctx.data(|d| d.get_temp::<Change>(id));
    match &mut state {
        None => {
            ctx.data_mut(|d| {
                d.insert_temp(
                    id,
                    Change {
                        current: key.to_owned(),
                        previous: None,
                        at: now - duration,
                    },
                )
            });
            return None;
        }
        Some(s) if s.current != key => {
            s.previous = Some(std::mem::replace(&mut s.current, key.to_owned()));
            s.at = now;
            ctx.data_mut(|d| d.insert_temp(id, s.clone()));
        }
        Some(_) => {}
    }
    let s = state?;
    let t = ((now - s.at) / duration) as f32;
    if t >= 1.0 {
        return None;
    }
    ctx.request_repaint();
    s.previous.map(|p| (p, t.max(0.0)))
}

/// Slides a rect by a fraction of its own size, for rolls and handoffs.
pub fn offset(rect: Rect, dx: f32, dy: f32) -> Rect {
    rect.translate(vec2(rect.width() * dx, rect.height() * dy))
}
