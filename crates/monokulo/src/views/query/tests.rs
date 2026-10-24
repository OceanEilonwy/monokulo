use super::*;

#[test]
fn a_long_json_value_is_pretty_printed_whole() {
    let content = value(
        "payload",
        "wd_1a2b",
        &Cell::Text {
            text: r#"{"event":"order.paid"}"#.into(),
            len: 1_843,
        },
    );
    let html = value_dialog(content).into_string();
    assert!(html.contains("wd_1a2b · 1.8 KB · JSON"), "{html}");
    assert!(
        html.contains("&quot;event&quot;: &quot;order.paid&quot;"),
        "{html}"
    );
}
