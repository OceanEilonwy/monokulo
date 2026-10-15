//! The site's on/off switch. Dropdowns are the shared Select component
//! (`ui::Select`, with `ui::Choice` options).

use maud::{html, Markup};

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

    /// The site has one dropdown, the shared Select: no page writes a
    /// `select` of its own.
    #[test]
    fn every_select_is_the_dropdown_component() {
        let bare: Vec<String> = selects()
            .into_iter()
            .map(|(file, line, text, _)| format!("{file}:{line}: {}", text.trim()))
            .collect();
        assert!(
            bare.is_empty(),
            "use ui::Select for these:\n{}",
            bare.join("\n")
        );
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
