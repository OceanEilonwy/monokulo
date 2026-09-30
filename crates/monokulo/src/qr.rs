//! QR codes as inline SVG. [`QrSvg`] is the only raw markup a page takes
//! from a runtime value, so it can only come from [`encode`]: nothing else
//! can put text into a page unescaped by calling it a QR code.

use qrcode::render::svg;
use qrcode::QrCode;

/// An SVG QR code, drawn by [`encode`]. Rendered into a page as is, and
/// serialized as its markup (the POS app draws the redrawn code it is sent).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct QrSvg(String);

impl QrSvg {
    /// A stand-in for view tests, which check the page around the code.
    #[cfg(test)]
    pub fn for_test(markup: &str) -> Self {
        QrSvg(markup.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl maud::Render for QrSvg {
    fn render(&self) -> maud::Markup {
        maud::PreEscaped(self.0.clone())
    }
}

/// `data` as a QR code: just the `<svg>` element (no XML prolog), hidden
/// from screen readers, since the address it encodes is shown as text beside
/// it.
pub fn encode(data: &str) -> Result<QrSvg, qrcode::types::QrError> {
    let full = QrCode::new(data.as_bytes())?.render::<svg::Color>().build();
    let svg = match full.find("<svg") {
        Some(idx) => &full[idx..],
        None => &full[..],
    };
    Ok(QrSvg(svg.replacen(
        "<svg",
        r#"<svg role="presentation" aria-hidden="true" focusable="false""#,
        1,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_one_svg_element_hidden_from_screen_readers() {
        let svg = encode("monero:4abc?tx_amount=1").unwrap();
        assert!(svg
            .as_str()
            .starts_with(r#"<svg role="presentation" aria-hidden="true""#));
        assert!(svg.as_str().trim_end().ends_with("</svg>"));
    }
}
