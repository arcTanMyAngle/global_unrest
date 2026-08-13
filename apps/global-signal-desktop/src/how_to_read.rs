//! "How to read this map" — the first-run and `?`-key overlay
//! (docs/VISUALIZATION.md V3 item 10).
//!
//! The caveats in `docs/SAFETY_AND_PRIVACY.md` are the honest part of this
//! project, and a caveat nobody reads is not a caveat. This puts them where
//! people actually look: in front of the map, once, before they have drawn a
//! conclusion from it.
//!
//! It says what the map *cannot* tell you as prominently as what it can — the
//! limits are not an appendix here, they are a section of equal weight.
//! Nothing in this window is generated from the data; it is a fixed
//! explanation of the encodings, kept next to the encodings it describes.

use egui::{Color32, RichText, Ui};

/// Settings key recording that the overlay has been dismissed once. Versioned
/// so a future rewrite worth re-showing can bump it rather than silently
/// reusing a dismissal of different text.
pub const SEEN_KEY: &str = "how_to_read_seen_v1";

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
const TEXT_LEAD: Color32 = Color32::from_rgb(216, 221, 231);

/// One paragraph: an optional bold lead-in, then the sentence.
///
/// The split is structural rather than markup because `egui::RichText` does
/// **not** parse markdown — a `**bold**` written inline renders its asterisks
/// literally. Styling has to be applied per span, so the spans are data.
struct Para {
    lead: &'static str,
    text: &'static str,
}

struct Section {
    heading: &'static str,
    paras: &'static [Para],
}

const SECTIONS: [Section; 5] = [
    Section {
        heading: "What this map shows",
        paras: &[
            Para {
                lead: "Two different things, kept apart on purpose. ",
                text: "Media attention is how much coverage a place is getting. Event \
                       data is discrete reported incidents — protests, armed clashes, \
                       disruptions. They are computed separately, shown separately, and \
                       never merged into one \"activity\" number, because a place can be \
                       loudly covered and quiet, or busy and ignored.",
            },
            Para {
                lead: "",
                text: "Every score is broken into its components. Where a combined \
                       figure appears, its parts are always shown beside it.",
            },
        ],
    },
    Section {
        heading: "Where things are drawn, and why some are not points",
        paras: &[
            Para {
                lead: "",
                text: "A record only becomes a point marker if its source gave it a \
                       city-level or exact location. Anything coarser — a country, a \
                       state — shades the region instead.",
            },
            Para {
                lead: "That is not a style choice. ",
                text: "A country-precision record has no coordinate anyone observed, so \
                       putting a dot at the country's centroid would invent a location \
                       and make a guess look like an observation. NOAA weather alerts \
                       and IODA internet outages are region-only for this reason.",
            },
        ],
    },
    Section {
        heading: "Reading a marker",
        paras: &[
            Para {
                lead: "Color is what kind of thing it is; shape is which feed reported \
                       it. ",
                text: "A diamond is ACLED, a square is GDELT, and the two triangles are \
                       the Bluesky and Telegram chatter feeds. Size follows severity \
                       where the source reports one, and article count otherwise.",
            },
            Para {
                lead: "A pulsing ring is a spike halo. ",
                text: "That cell is clearly above its own trailing baseline. Cells too \
                       new to have a baseline never get one — no history, no anomaly \
                       claim.",
            },
            Para {
                lead: "",
                text: "The full encoding reference is in the Legend at the bottom of \
                       the right-hand panel.",
            },
        ],
    },
    Section {
        heading: "What this map cannot tell you",
        paras: &[
            Para {
                lead: "Coverage is not reality. ",
                text: "Media density varies enormously by language, region and news \
                       cycle. A quiet cell may be a quiet place, or a place our sources \
                       do not index. The attention-vs-unrest layer is a picture of that \
                       gap — of our own coverage — not a picture of the world.",
            },
            Para {
                lead: "Absence is not evidence. ",
                text: "NOAA alerts are United States only. ACLED and GDELT each have \
                       their own coverage windows and revision schedules. A blank \
                       region usually means nobody in our sources reported anything.",
            },
            Para {
                lead: "Nothing here is about people. ",
                text: "Social sources are counted, never stored: no post, no author, no \
                       text ever reaches the database — only how many matching posts \
                       mentioned a place and a topic in a time window. Places are \
                       matched against a gazetteer, never inferred about a person.",
            },
            Para {
                lead: "Time windows change everything. ",
                text: "Ranked views rank within the visible window only. Move the \
                       window and every cell can move with it.",
            },
        ],
    },
    Section {
        heading: "Getting around",
        paras: &[
            Para {
                lead: "",
                text: "Drag to pan, scroll to zoom, click a cell to inspect it. The \
                       timeline strip along the bottom sets the window and plays it \
                       forward.",
            },
            Para {
                lead: "",
                text: "Press ? at any time to bring this back, or use \"how to read \
                       this\" in the top bar.",
            },
        ],
    },
];

/// Draw the overlay's contents. Returns true if the user asked to close it.
pub fn show(ui: &mut Ui) -> bool {
    let mut close = false;
    ui.label(
        RichText::new(
            "This map is built to be read carefully. A minute here will stop you \
             drawing a conclusion the data does not support.",
        )
        .color(TEXT_DIM),
    );
    ui.add_space(8.0);
    egui::ScrollArea::vertical()
        .max_height(440.0)
        .show(ui, |ui| {
            for section in &SECTIONS {
                ui.add_space(6.0);
                ui.label(RichText::new(section.heading).strong());
                for para in section.paras {
                    ui.add_space(3.0);
                    // Wrapped horizontal layout so the bold lead-in and the
                    // sentence flow as one paragraph rather than two blocks.
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if !para.lead.is_empty() {
                            ui.label(RichText::new(para.lead).strong().color(TEXT_LEAD));
                        }
                        ui.label(RichText::new(para.text).color(TEXT_DIM));
                    });
                }
                ui.add_space(4.0);
                ui.separator();
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "The full policy is in docs/SAFETY_AND_PRIVACY.md; the design \
                     rationale for every view is in docs/VISUALIZATION.md.",
                )
                .color(TEXT_DIM)
                .small(),
            );
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Got it").clicked() {
            close = true;
        }
        ui.label(
            RichText::new("or press ? to reopen")
                .color(TEXT_DIM)
                .small(),
        );
    });
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits section is the reason this overlay exists; a refactor that
    /// trims it to a footnote should fail here rather than ship quietly.
    #[test]
    fn the_limits_section_is_not_an_afterthought() {
        let limits = SECTIONS
            .iter()
            .find(|s| s.heading.contains("cannot"))
            .expect("a section on what the map cannot tell you");
        assert!(
            limits.paras.len() >= 4,
            "the limits section lost paragraphs: {}",
            limits.paras.len()
        );
        let text: String = limits.paras.iter().map(|p| p.lead).collect();
        for topic in ["Coverage", "Absence", "people", "Time windows"] {
            assert!(text.contains(topic), "limits no longer lead with {topic}");
        }
    }

    #[test]
    fn every_section_has_a_heading_and_a_body() {
        for s in &SECTIONS {
            assert!(!s.heading.is_empty());
            assert!(!s.paras.is_empty(), "{} has no body", s.heading);
            assert!(s.paras.iter().all(|p| p.text.len() > 40), "{}", s.heading);
        }
    }

    /// `RichText` renders markup literally, so any `*` or `_` emphasis that
    /// creeps back into the copy would show up on screen as punctuation.
    #[test]
    fn copy_contains_no_markdown_markup() {
        for s in &SECTIONS {
            for p in s.paras {
                for span in [p.lead, p.text] {
                    assert!(!span.contains('*'), "markdown emphasis in: {span}");
                    assert!(!span.contains("__"), "markdown emphasis in: {span}");
                }
            }
        }
    }

    /// A lead-in runs straight into its sentence, so it has to end with the
    /// space that separates them — item spacing is zeroed in that layout.
    #[test]
    fn lead_ins_end_with_a_separating_space() {
        for s in &SECTIONS {
            for p in s.paras {
                if !p.lead.is_empty() {
                    assert!(p.lead.ends_with(' '), "lead runs into text: {}", p.lead);
                }
            }
        }
    }
}
