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
    Tiny,
}

/// One centred column: who's asking, the amount, where the order stands, the
/// code, the address, anything that's gone wrong, then everything else.
const DETAILED: &str = r#"{{merchant}}
<div class="pay-grid">
  <div class="pay-col-primary">
    {{amount bare}}
    <div class="pay-sub">{{fiat}}{{countdown inline}}</div>
    {{status track}}
    {{qr}}
    {{address plain}}
    {{message full}}
  </div>
  <div class="pay-col-secondary">
    {{refund}}
    {{progress text}}
    {{payments folded}}
    <div class="meta">Order ID: {{order-id}}</div>
  </div>
</div>
"#;

/// A card in a 280 by 380 frame: the amount and status symbol, the code, then
/// the short address and a few words.
const COMPACT: &str = r#"<div class="pass">
  <div class="pass-head">
    <div class="pass-amounts">{{amount bare}}{{fiat}}</div>
    {{status icon}}
  </div>
  <div class="pass-code">{{qr medium}}</div>
  <div class="pass-foot">
    {{address short}}
    {{message}}
  </div>
</div>
"#;

/// The code with everything else beside it, in a 280 by 152 frame. Nothing
/// moves as the order changes: the same parts stay, in every state.
const TINY: &str = r#"<div class="tiny">
  {{qr small}}
  <div class="tiny-side">
    <div class="tiny-amounts">{{amount bare}}{{fiat}}</div>
    {{status label}}
    {{message}}
    {{address short}}
  </div>
</div>
"#;

impl Layout {
    pub const ALL: [Layout; 3] = [Layout::Detailed, Layout::Compact, Layout::Tiny];

    /// What `?view=` calls it.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Detailed => "detailed",
            Layout::Compact => "compact",
            Layout::Tiny => "tiny",
        }
    }

    pub fn from_name(name: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|layout| layout.name() == name)
    }

    /// What a merchant knows it as.
    pub fn label(self) -> &'static str {
        match self {
            Layout::Detailed => "Detailed",
            Layout::Compact => "Compact",
            Layout::Tiny => "Tiny",
        }
    }

    /// Its template, as written: what "Start from" gives a custom page.
    pub fn source(self) -> &'static str {
        match self {
            Layout::Detailed => DETAILED,
            Layout::Compact => COMPACT,
            Layout::Tiny => TINY,
        }
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
        static TINY_PARSED: LazyLock<Template> = LazyLock::new(|| parsed(TINY));
        match self {
            Layout::Detailed => &DETAILED_PARSED,
            Layout::Compact => &COMPACT_PARSED,
            Layout::Tiny => &TINY_PARSED,
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
