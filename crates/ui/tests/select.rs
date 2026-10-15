//! The Select's options: their text says everything in words, for a browser
//! without JavaScript, and their data attributes give the enhanced list its
//! parts.

use maud::Render;
use ui::{Choice, Select};

fn option(choice: Choice) -> String {
    let html = Select::new("x", vec![choice]).render().into_string();
    let start = html.find("<option").expect("an option");
    let end = html.find("</option>").expect("its end") + "</option>".len();
    html[start..end].to_string()
}

#[test]
fn an_option_says_everything_in_words_and_gives_the_list_its_parts() {
    assert_eq!(
        option(
            Choice::new("w_1", "Copper Heron")
                .detail("48xQ7…v3Rk")
                .current(true)
                .note("since 1 Sep 2026")
                .selected(true)
        ),
        r#"<option value="w_1" selected data-label="Copper Heron" data-detail="48xQ7…v3Rk" data-chip="Current" data-chip-tone="current" data-note="since 1 Sep 2026">Copper Heron (48xQ7…v3Rk) [Current] - since 1 Sep 2026</option>"#
    );
}

#[test]
fn an_option_names_its_network_in_words_and_for_the_badge() {
    assert_eq!(
        option(
            Choice::new("w_2", "Feather test")
                .network("stagenet")
                .note("1 store")
        ),
        r#"<option value="w_2" data-label="Feather test" data-network="stagenet" data-note="1 store">Feather test [Stagenet] - 1 store</option>"#
    );
}

/// The setup flow's network dropdown: the badge alone, `[Stagenet]`
/// without JavaScript.
#[test]
fn a_network_on_its_own_is_its_badge() {
    assert_eq!(
        option(
            Choice::new("stagenet", "")
                .network("stagenet")
                .selected(true)
        ),
        r#"<option value="stagenet" selected data-label="" data-network="stagenet">[Stagenet]</option>"#
    );
}

/// A logo is drawn by the enhanced list; the words don't change.
#[test]
fn a_logo_is_for_the_list_only() {
    assert_eq!(
        option(Choice::new("feather", "Feather").logo("/static/wallet-logos/feather.png")),
        r#"<option value="feather" data-label="Feather" data-logo="/static/wallet-logos/feather.png">Feather</option>"#
    );
}

#[test]
fn a_plain_option_is_a_plain_option() {
    assert_eq!(
        option(Choice::new("info", "Info and up")),
        r#"<option value="info">Info and up</option>"#
    );
    assert_eq!(
        option(Choice::prompt("Choose a wallet…", true)),
        r#"<option value="" selected disabled>Choose a wallet…</option>"#
    );
}

#[test]
fn only_current_is_green() {
    let plain = option(Choice::new("EUR", "Euro").chip("Store's"));
    assert!(plain.contains(r#"data-chip="Store's""#), "{plain}");
    assert!(!plain.contains("data-chip-tone"), "{plain}");
}

/// The server renders the select an island attaches to: the select with
/// a mark to find it by, and a marker where the button and list go. Its
/// root is the island: it names the component and carries its props.
#[test]
fn a_select_is_an_island_around_a_working_select() {
    let html = Select::new("network", vec![Choice::new("mainnet", "Mainnet")])
        .id("net")
        .required(true)
        .attr("aria-describedby", "net-help")
        .render()
        .into_string();
    assert_eq!(
        html,
        concat!(
            r#"<div class="mk-select" data-mk="1" data-island="Select" data-props="{&quot;search&quot;:&quot;auto&quot;}">"#,
            r#"<select class="mk-input" name="network" id="net" required data-mk="2" aria-describedby="net-help">"#,
            r#"<option value="mainnet">Mainnet</option></select><!--mk:3--></div>"#
        )
    );
}
