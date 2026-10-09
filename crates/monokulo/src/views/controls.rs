//! The site's dropdown and on/off switch (docs/dropdowns.md).
//!
//! Every dropdown is a plain `<select>` inside `<mk-select>`
//! (`static/mk-select.js`), its options written with [`Choice`]: the
//! option's text says everything in words, for a browser without
//! JavaScript, and its data attributes give the component the parts to
//! draw (a label, a monospace detail, a network badge, a chip, a note).

use maud::{html, Markup, Render};

/// One option of a dropdown.
#[derive(Debug, Clone, Default)]
pub struct Choice {
    value: String,
    label: String,
    detail: Option<String>,
    network: Option<String>,
    chip: Option<Chip>,
    note: Option<String>,
    selected: bool,
    disabled: bool,
}

/// A short tag after an option's label. Only [`Chip::Current`] is
/// coloured: green means one thing, the one in use now.
#[derive(Debug, Clone)]
pub enum Chip {
    Current,
    Plain(String),
}

impl Chip {
    fn text(&self) -> &str {
        match self {
            Chip::Current => "Current",
            Chip::Plain(text) => text,
        }
    }
}

impl Choice {
    pub fn new(value: impl AsRef<str>, label: impl AsRef<str>) -> Choice {
        Choice {
            value: value.as_ref().to_owned(),
            label: label.as_ref().to_owned(),
            ..Choice::default()
        }
    }

    /// The "Choose…" prompt of a required dropdown with nothing picked
    /// yet: no value, never pickable, shown until something is.
    pub fn prompt(label: impl AsRef<str>, shown: bool) -> Choice {
        Choice::new("", label).disabled(true).selected(shown)
    }

    /// Monospace and muted after the label: an address, a currency code.
    pub fn detail(mut self, detail: impl AsRef<str>) -> Choice {
        self.detail = Some(detail.as_ref().to_owned());
        self
    }

    /// A wallet's Monero network (`mainnet`, `stagenet`, `testnet`), drawn
    /// as the site's network badge (`views::network_badge`).
    pub fn network(mut self, network: impl AsRef<str>) -> Choice {
        self.network = Some(network.as_ref().to_owned());
        self
    }

    pub fn chip(mut self, chip: Chip) -> Choice {
        self.chip = Some(chip);
        self
    }

    /// The green Current chip, when `current`.
    pub fn current(mut self, current: bool) -> Choice {
        if current {
            self.chip = Some(Chip::Current);
        }
        self
    }

    /// Muted, at the end: a count, a date, why it can't be picked.
    pub fn note(mut self, note: impl AsRef<str>) -> Choice {
        self.note = Some(note.as_ref().to_owned());
        self
    }

    pub fn selected(mut self, selected: bool) -> Choice {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Choice {
        self.disabled = disabled;
        self
    }

    /// The option's text, as a browser without JavaScript shows it:
    /// `Label (detail) [Network] [chip] - note`.
    pub fn text(&self) -> String {
        let mut text = self.label.clone();
        if let Some(detail) = &self.detail {
            text.push_str(&format!(" ({detail})"));
        }
        if let Some(network) = &self.network {
            text.push_str(&format!(" [{}]", super::network_word(network)));
        }
        if let Some(chip) = &self.chip {
            text.push_str(&format!(" [{}]", chip.text()));
        }
        if let Some(note) = &self.note {
            text.push_str(&format!(" - {note}"));
        }
        text
    }

    fn has_parts(&self) -> bool {
        self.detail.is_some()
            || self.network.is_some()
            || self.chip.is_some()
            || self.note.is_some()
    }
}

impl Render for Choice {
    fn render(&self) -> Markup {
        let parts = self.has_parts();
        html! {
            option value=(self.value) selected[self.selected] disabled[self.disabled]
                data-label=[parts.then_some(&self.label)]
                data-detail=[self.detail.as_ref()]
                data-network=[self.network.as_ref()]
                data-chip=[self.chip.as_ref().map(Chip::text)]
                data-chip-tone=[matches!(self.chip, Some(Chip::Current)).then_some("current")]
                data-note=[self.note.as_ref()] {
                (self.text())
            }
        }
    }
}

/// An on/off setting: a checkbox drawn as a switch, sent as `true` when on.
/// An unticked checkbox sends nothing, so a hidden `switches` field names
/// it, and the form handler reads its absence as `false`.
/// `saved`, when a refused save shows another value: what is saved, for
/// the admin page's script to tell the switch is still unsaved.
pub fn switch(
    name: &str,
    id: &str,
    on: bool,
    described_by: Option<&str>,
    saved: Option<bool>,
) -> Markup {
    let saved = saved.map(|on| if on { "on" } else { "off" });
    html! {
        input type="hidden" name="switches" value=(name);
        label class="switch" {
            input type="checkbox" role="switch" name=(name) value="true" id=(id) checked[on] aria-describedby=[described_by] data-saved=[saved];
            span class="switch-track" aria-hidden="true" {}
            span class="switch-on" aria-hidden="true" { "On" }
            span class="switch-off" aria-hidden="true" { "Off" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_option_says_everything_in_words_and_gives_the_component_its_parts() {
        let html = Choice::new("w_1", "Copper Heron")
            .detail("48xQ7…v3Rk")
            .current(true)
            .note("since 1 Sep 2026")
            .selected(true)
            .render()
            .into_string();
        assert_eq!(
            html,
            r#"<option value="w_1" selected data-label="Copper Heron" data-detail="48xQ7…v3Rk" data-chip="Current" data-chip-tone="current" data-note="since 1 Sep 2026">Copper Heron (48xQ7…v3Rk) [Current] - since 1 Sep 2026</option>"#
        );
    }

    #[test]
    fn an_option_names_its_network_in_words_and_for_the_badge() {
        let html = Choice::new("w_2", "Feather test")
            .network("stagenet")
            .note("1 store")
            .render()
            .into_string();
        assert_eq!(
            html,
            r#"<option value="w_2" data-label="Feather test" data-network="stagenet" data-note="1 store">Feather test [Stagenet] - 1 store</option>"#
        );
    }

    #[test]
    fn a_plain_option_is_a_plain_option() {
        assert_eq!(
            Choice::new("info", "Info and up").render().into_string(),
            r#"<option value="info">Info and up</option>"#
        );
        assert_eq!(
            Choice::prompt("Choose a wallet…", true)
                .render()
                .into_string(),
            r#"<option value="" selected disabled>Choose a wallet…</option>"#
        );
    }

    #[test]
    fn only_current_is_green() {
        let plain = Choice::new("EUR", "Euro")
            .chip(Chip::Plain("Store's".into()))
            .render()
            .into_string();
        assert!(plain.contains(r#"data-chip="Store's""#), "{plain}");
        assert!(!plain.contains("data-chip-tone"), "{plain}");
    }

    /// Every Maud `select` in the views and handlers: file, line number,
    /// the line and the line before it.
    fn selects() -> Vec<(String, usize, String, String)> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        for dir in ["src/views", "src/http"] {
            for entry in std::fs::read_dir(root.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let lines: Vec<&str> = source.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    let code = line.trim_start();
                    // Maud's `select name=…` / `select id=…`, at the start
                    // of a line or straight after a `{`.
                    let element = ["select name=", "select id="]
                        .iter()
                        .any(|tag| code.starts_with(tag) || code.contains(&format!("{{ {tag}")));
                    if element {
                        let before = if i > 0 {
                            lines[i - 1].to_string()
                        } else {
                            String::new()
                        };
                        found.push((path.display().to_string(), i + 1, line.to_string(), before));
                    }
                }
            }
        }
        found
    }

    /// The site has one dropdown: a select is always inside `<mk-select>`.
    #[test]
    fn every_select_is_the_dropdown_component() {
        let bare: Vec<String> = selects()
            .into_iter()
            .filter(|(_, _, line, before)| {
                !line.contains("mk-select {")
                    && !before.trim_end().ends_with("mk-select {")
                    && !before.trim_end().ends_with("mk-select compact {")
            })
            .map(|(file, line, text, _)| format!("{file}:{line}: {}", text.trim()))
            .collect();
        assert!(
            bare.is_empty(),
            "wrap these in mk-select:\n{}",
            bare.join("\n")
        );
        assert!(selects().len() >= 12, "the scan finds the selects");
    }

    #[test]
    fn a_switch_is_named_so_off_is_sent_too() {
        let html =
            switch("payment.accept_unconfirmed", "setting-x", false, None, None).into_string();
        assert!(
            html.contains(
                r#"<input type="hidden" name="switches" value="payment.accept_unconfirmed">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"type="checkbox" role="switch" name="payment.accept_unconfirmed" value="true""#
            ),
            "{html}"
        );
        assert!(!html.contains("checked"), "{html}");
        assert!(switch("x", "x", true, None, None)
            .into_string()
            .contains("checked"));
    }
}
