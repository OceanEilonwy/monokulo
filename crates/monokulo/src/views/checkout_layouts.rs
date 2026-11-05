//! The stock checkout layouts, which are templates like any merchant's and
//! pass the same checks (`checkout_template::check`), so the engine custom
//! pages run on is exercised by every checkout, and "Start from" in the
//! custom page wizard starts from what these really are.

use std::sync::LazyLock;

use checkout_template::Template;

/// A checkout's layout, as `?view=` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Detailed,
    Compact,
}

const DETAILED: &str = r#"{{merchant}}
<div class="pay-grid">
  {{status}}
  <div class="pay-col-primary">
    {{amount}}
    {{fiat}}
    {{countdown}}
    {{qr}}
    {{address}}
  </div>
  <div class="pay-col-secondary">
    {{progress}}
    {{refund}}
    {{payments}}
    <div class="meta">Order ID: {{order-id}}</div>
  </div>
</div>
"#;

const COMPACT: &str = r#"{{merchant}}
<div class="pay-grid">
  {{status}}
  <div class="pay-col-primary">
    {{amount}}
    {{fiat}}
    {{countdown}}
    {{qr}}
    {{address}}
  </div>
  <div class="pay-col-secondary">
    {{refund}}
  </div>
</div>
"#;

impl Layout {
    pub const ALL: [Layout; 2] = [Layout::Detailed, Layout::Compact];

    /// What `?view=` calls it.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Detailed => "detailed",
            Layout::Compact => "compact",
        }
    }

    pub fn from_name(name: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|layout| layout.name() == name)
    }

    /// Its template, parsed once. A test holds every layout to the checks a
    /// merchant's must pass, so none of these can fail on a running server.
    pub fn template(self) -> &'static Template {
        fn parsed(source: &str) -> Template {
            checkout_template::check(source)
                .unwrap_or_else(|problems| panic!("a stock layout: {problems:?}"))
        }
        static DETAILED_PARSED: LazyLock<Template> = LazyLock::new(|| parsed(DETAILED));
        static COMPACT_PARSED: LazyLock<Template> = LazyLock::new(|| parsed(COMPACT));
        match self {
            Layout::Detailed => &DETAILED_PARSED,
            Layout::Compact => &COMPACT_PARSED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_passes_what_a_merchants_template_must() {
        for layout in Layout::ALL {
            layout.template();
            assert_eq!(Layout::from_name(layout.name()), Some(layout));
        }
        assert_eq!(Layout::from_name("nope"), None);
    }
}
