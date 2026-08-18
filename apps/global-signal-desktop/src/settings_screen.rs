//! "Settings" — per-source state, and the one control that changes it.
//!
//! Three things can stop a source producing data, and conflating them is what
//! makes a dark feed unexplainable:
//!
//! - **compiled in** — the desktop was built with that source's `*-live`
//!   Cargo feature. A build without it has no network path at all, and no
//!   amount of configuration will change that.
//! - **configured** — the credentials it needs are present in the process
//!   environment. Keyless sources are always configured.
//! - **switched on** — the user has not turned it off here.
//!
//! Plus the global live-updates pause, which is deliberately *not* a per-
//! source setting: it is one switch in the top bar, and a source turned off
//! here stays off across a pause/resume cycle.
//!
//! Credential values never appear on this screen, in any form — not a value,
//! not a masked prefix, not a length, not a "looks valid" hint. Only the env
//! var *name* and a yes/no. That is CLAUDE.md product rule 5, and it is why
//! this module never touches `std::env::var`'s return value beyond the
//! emptiness check `SourceAttribution::is_configured` already does.
//!
//! Everything rendered here is already-owned UI state: `App::source_statuses`
//! is fed by the ingest worker's existing status channel, so drawing this
//! window issues no query and blocks no frame.

use core_types::{AttributionSubject, SourceAttribution, SourceId, attribution_for};
use egui::{Color32, RichText, Ui};

use crate::app::App;
use crate::ingest::{self, SourceStatus};

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
const TEXT_LEAD: Color32 = Color32::from_rgb(216, 221, 231);
const OK: Color32 = Color32::from_rgb(126, 200, 140);
const WARN: Color32 = Color32::from_rgb(255, 196, 110);
const OFF: Color32 = Color32::from_rgb(130, 136, 148);

/// Live sources the desktop schedules, in the order they appear on screen.
/// Fixtures are excluded on purpose: the desktop never runs them, so a row
/// for them would offer a switch that controls nothing.
const SCHEDULED: [SourceId; 6] = [
    SourceId::Gdelt,
    SourceId::Acled,
    SourceId::Noaa,
    SourceId::Ioda,
    SourceId::Bluesky,
    SourceId::Telegram,
];

/// The on-demand third-party legs. Not sources: they have no cadence, no
/// status line, and nothing to switch — they run only when a person asks
/// (CLAUDE.md: "Media search is never scheduled"). They are listed so a
/// person can see whether the credentials for them are in place.
const ON_DEMAND: [AttributionSubject; 4] = [
    AttributionSubject::GoogleGemini,
    AttributionSubject::MediaGdelt,
    AttributionSubject::MediaBluesky,
    AttributionSubject::MediaTelegram,
];

/// Was this leg's Cargo feature compiled into *this* binary?
///
/// The attribution table stores the feature's name; only the desktop crate
/// can answer whether it was enabled, because `cfg!` is evaluated where it is
/// written. An unrecognised name reports "not compiled" rather than
/// optimistically claiming otherwise — and `every_feature_flag_is_known`
/// below fails the build's test run if one ever appears.
fn compiled_in(flag: Option<&str>) -> bool {
    match flag {
        // No feature gates this leg: it is always built.
        None => true,
        Some("acled-live") => cfg!(feature = "acled-live"),
        Some("noaa-live") => cfg!(feature = "noaa-live"),
        Some("ioda-live") => cfg!(feature = "ioda-live"),
        Some("bluesky-live") => cfg!(feature = "bluesky-live"),
        Some("telegram-live") => cfg!(feature = "telegram-live"),
        Some("gemini-live") => cfg!(feature = "gemini-live"),
        Some("media-live") => cfg!(feature = "media-live"),
        Some(_) => false,
    }
}

/// Nominal cadence as words. Rendered from `ingest::cadence_secs` rather than
/// re-stated here, so the screen cannot claim an interval the worker does not
/// use.
fn fmt_cadence(secs: u64) -> String {
    if secs.is_multiple_of(3600) {
        format!("every {}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("every {}m", secs / 60)
    } else {
        format!("every {secs}s")
    }
}

fn fmt_ts(epoch_s: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_s, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| format!("t={epoch_s}"))
}

/// Compact relative time vs. `now`, both epoch seconds.
fn fmt_relative(epoch_s: i64, now: i64) -> String {
    let d = epoch_s - now;
    let mag = d.unsigned_abs();
    let unit = if mag < 60 {
        format!("{mag}s")
    } else if mag < 3600 {
        format!("{}m", mag / 60)
    } else if mag < 86_400 {
        format!("{}h", mag / 3600)
    } else {
        format!("{}d", mag / 86_400)
    };
    if d < 0 {
        format!("{unit} ago")
    } else {
        format!("in {unit}")
    }
}

/// One `label: value` line, dim label, so the rows line up down the window.
fn field(ui: &mut Ui, label: &str, value: RichText) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 14.0],
            egui::Label::new(RichText::new(label).small().color(TEXT_DIM)),
        );
        ui.label(value.small());
    });
}

/// The single sentence that explains why a source is or is not fetching.
///
/// Order matters: it reports the *first* thing that would have to change,
/// so a person fixes one problem at a time instead of setting a key for a
/// source their build cannot use.
fn readiness(
    attr: &SourceAttribution,
    built: bool,
    enabled: bool,
    online: bool,
) -> (String, Color32) {
    if !built {
        let flag = attr.feature_flag.unwrap_or("(none)");
        return (
            format!("not compiled in — this build lacks the `{flag}` feature"),
            OFF,
        );
    }
    if !attr.is_configured() {
        let names = attr.env_vars.join(", ");
        return (
            format!("not configured — set {names} in the environment"),
            WARN,
        );
    }
    if !enabled {
        return ("switched off here".to_string(), OFF);
    }
    if !online {
        return ("ready — live updates are paused".to_string(), OFF);
    }
    ("fetching on cadence".to_string(), OK)
}

/// Credential state as a phrase — env var names only, never a value.
fn credential_line(attr: &SourceAttribution) -> (String, Color32) {
    if !attr.credentials_required {
        return ("no credentials needed".to_string(), TEXT_DIM);
    }
    let names = attr.env_vars.join(", ");
    if attr.is_configured() {
        (format!("configured · reads {names}"), OK)
    } else {
        (format!("not configured · reads {names}"), WARN)
    }
}

/// Draw the whole screen. Returns the toggle the user just flipped, if any —
/// applying it needs `&mut App`, and the rows borrow `App` immutably to read
/// their status, so the decision is returned rather than made in place.
#[must_use]
pub fn show(app: &App, ui: &mut Ui) -> Option<(SourceId, bool)> {
    let mut toggled = None;
    let now = chrono::Utc::now().timestamp();

    ui.label(
        RichText::new(
            "Where each feed stands right now, and what would have to change \
             for a dark one to start reporting.",
        )
        .color(TEXT_DIM),
    );
    ui.add_space(2.0);
    ui.label(
        RichText::new(
            "Credentials live in the environment and are never stored or shown \
             here — only the variable name and whether it is set.",
        )
        .color(TEXT_DIM)
        .small(),
    );
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .max_height(460.0)
        .show(ui, |ui| {
            for source in SCHEDULED {
                let attr = attribution_for(AttributionSubject::Source(source));
                let status = app.source_statuses.iter().find(|s| s.source == source);
                source_row(ui, app, source, &attr, status, now, &mut toggled);
                ui.add_space(4.0);
                ui.separator();
            }

            ui.add_space(6.0);
            ui.label(RichText::new("On request only").strong());
            ui.label(
                RichText::new(
                    "These never poll. They run when you ask for a Daily Events \
                     digest or a Media search, and there is nothing to schedule \
                     or switch off.",
                )
                .color(TEXT_DIM)
                .small(),
            );
            for subject in ON_DEMAND {
                let attr = attribution_for(subject);
                let built = compiled_in(attr.feature_flag);
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(attr.display_name).color(TEXT_LEAD));
                    if !built {
                        let flag = attr.feature_flag.unwrap_or("(none)");
                        ui.label(
                            RichText::new(format!("not compiled (`{flag}`)"))
                                .small()
                                .color(OFF),
                        );
                    }
                });
                let (cred, colour) = credential_line(&attr);
                field(ui, "credentials", RichText::new(cred).color(colour));
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Switching a source off stops it fetching and persists across \
                     restarts. It does not delete anything already stored — use \
                     the retention control in the top bar for that.",
                )
                .color(TEXT_DIM)
                .small(),
            );
        });

    toggled
}

/// One scheduled source: name, switch, readiness, and its timing.
fn source_row(
    ui: &mut Ui,
    app: &App,
    source: SourceId,
    attr: &SourceAttribution,
    status: Option<&SourceStatus>,
    now: i64,
    toggled: &mut Option<(SourceId, bool)>,
) {
    let built = compiled_in(attr.feature_flag);
    // The persisted set is the authority for the switch, not the status line:
    // the status line can lag a frame behind a click, and a checkbox that
    // snaps back looks broken.
    let enabled = !app.disabled_sources.contains(&source);

    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // Disabled when the build cannot use the source at all: offering a
        // switch that changes nothing is worse than showing why it is dark.
        let mut on = enabled;
        let response = ui.add_enabled(built, egui::Checkbox::new(&mut on, ""));
        if response.changed() {
            *toggled = Some((source, on));
        }
        ui.label(RichText::new(attr.display_name).color(TEXT_LEAD));
        if let Some(url) = attr.homepage_url {
            ui.hyperlink_to(RichText::new("terms ↗").small(), url);
        }
    });

    let (state, colour) = readiness(attr, built, enabled, app.online);
    field(ui, "state", RichText::new(state).color(colour));

    let (cred, cred_colour) = credential_line(attr);
    field(ui, "credentials", RichText::new(cred).color(cred_colour));

    match ingest::cadence_secs(source) {
        Some(secs) => field(
            ui,
            "cadence",
            RichText::new(fmt_cadence(secs)).color(TEXT_DIM),
        ),
        None => field(
            ui,
            "cadence",
            RichText::new("not scheduled").color(TEXT_DIM),
        ),
    }

    let Some(status) = status else {
        // The worker sends a line per source at startup, so this is a
        // first-frames state rather than a missing feed.
        field(
            ui,
            "last fetch",
            RichText::new("no report from the worker yet").color(TEXT_DIM),
        );
        return;
    };

    field(
        ui,
        "last success",
        match status.last_success_epoch_s {
            Some(ts) => {
                RichText::new(format!("{} ({})", fmt_ts(ts), fmt_relative(ts, now))).color(TEXT_DIM)
            }
            None => RichText::new("never").color(TEXT_DIM),
        },
    );
    field(
        ui,
        "last attempt",
        match status.last_attempt_epoch_s {
            Some(ts) => {
                RichText::new(format!("{} ({})", fmt_ts(ts), fmt_relative(ts, now))).color(TEXT_DIM)
            }
            None => RichText::new("never").color(TEXT_DIM),
        },
    );
    field(
        ui,
        "next poll",
        match status.next_attempt_epoch_s {
            Some(ts) => RichText::new(fmt_relative(ts, now)).color(TEXT_DIM),
            None => RichText::new("not scheduled").color(TEXT_DIM),
        },
    );

    // The worker's own words for the last cycle: counts on success, the error
    // on failure. Shown verbatim so a failing source explains itself here
    // instead of only in the log.
    let detail_colour = if status.degraded || status.partial {
        WARN
    } else {
        TEXT_DIM
    };
    field(
        ui,
        "last report",
        RichText::new(status.detail.clone()).color(detail_colour),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `compiled_in` matches on feature names as strings, so a renamed or new
    /// feature would silently render "not compiled" forever. This is the test
    /// that catches it.
    #[test]
    fn every_feature_flag_is_known() {
        for subject in AttributionSubject::ALL {
            let attr = attribution_for(subject);
            let Some(flag) = attr.feature_flag else {
                continue;
            };
            assert!(
                matches!(
                    flag,
                    "acled-live"
                        | "noaa-live"
                        | "ioda-live"
                        | "bluesky-live"
                        | "telegram-live"
                        | "gemini-live"
                        | "media-live"
                ),
                "{flag} is in the attribution table but not in compiled_in()"
            );
        }
    }

    /// Every scheduled source has a cadence to show, and fixtures are not one
    /// of them.
    #[test]
    fn scheduled_sources_have_a_cadence() {
        for source in SCHEDULED {
            assert!(
                ingest::cadence_secs(source).is_some(),
                "{source:?} is listed as scheduled but has no cadence"
            );
        }
        assert!(!SCHEDULED.contains(&SourceId::Fixtures));
    }

    /// Product rule 5. The screen may name an env var; it may never render
    /// anything derived from its value — not the value, not a prefix, not a
    /// length, not a checksum. `credential_line` is the only place a var name
    /// is formatted, so this pins its output to strings built solely from the
    /// names: any future edit that folds in something value-derived fails
    /// here, on whichever branch this machine's environment happens to take.
    #[test]
    fn credential_line_is_built_only_from_env_var_names() {
        for subject in AttributionSubject::ALL {
            let attr = attribution_for(subject);
            let (line, _) = credential_line(&attr);
            if !attr.credentials_required {
                assert_eq!(line, "no credentials needed");
                continue;
            }
            let names = attr.env_vars.join(", ");
            assert!(
                line == format!("configured · reads {names}")
                    || line == format!("not configured · reads {names}"),
                "{subject:?} renders a credential line this test does not \
                 recognise, which is how a value would get on screen: {line}"
            );
        }
    }

    /// The unconfigured path is the one a person actually hits, so its wording
    /// has to name the variable to set rather than just saying "no".
    #[test]
    fn unconfigured_says_what_to_set() {
        let attr = attribution_for(AttributionSubject::Source(SourceId::Acled));
        assert!(attr.credentials_required);
        assert!(!attr.env_vars.is_empty());
        let (line, colour) = readiness(&attr, true, true, true);
        if attr.is_configured() {
            // A developer machine with real ACLED credentials present.
            assert_eq!(line, "fetching on cadence");
        } else {
            assert!(line.starts_with("not configured"), "{line}");
            for name in attr.env_vars {
                assert!(line.contains(name), "{line} omits {name}");
            }
            assert_eq!(colour, WARN);
        }
    }

    /// "Not compiled" outranks "not configured": setting a key for a source
    /// this build cannot call would be wasted effort.
    #[test]
    fn missing_feature_is_reported_before_missing_credentials() {
        let attr = attribution_for(AttributionSubject::Source(SourceId::Acled));
        let (line, _) = readiness(&attr, false, true, true);
        assert!(line.starts_with("not compiled in"), "{line}");
    }

    #[test]
    fn cadence_reads_as_words() {
        assert_eq!(fmt_cadence(43_200), "every 12h");
        assert_eq!(fmt_cadence(900), "every 15m");
        assert_eq!(fmt_cadence(90), "every 90s");
    }
}
