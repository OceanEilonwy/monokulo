//! Screens: every screen the browser tests photographed, grouped by test
//! group. Each card links its full-size image; with JavaScript the device
//! and theme switches, the search and the side-by-side viewer work too.

use super::heading;
use crate::pages::format::{capitalised, count, plural};
use crate::pages::inputs::{Image, Screen, Shape, Theme};
use crate::pages::model::Report;
use maud::{html, Markup, PreEscaped};
use serde::Serialize;
use std::collections::BTreeMap;

/// The test groups' names on the page.
fn group_name(group: &str) -> String {
    match group {
        "checkout" => "Checkout".into(),
        "hosted-payment" => "Hosted payment page".into(),
        "pos" => "Point of sale".into(),
        "pos-timeline" => "POS timeline".into(),
        "site" => "Dashboard".into(),
        "admin-settings" => "Admin settings".into(),
        "store-settings" => "Store settings".into(),
        "logs" => "Logs".into(),
        "challenge" => "Connection challenge".into(),
        "engine" => "Engine status".into(),
        other => capitalised(&other.replace('-', " ")),
    }
}

/// `checkout-paid-in-full` of group `checkout` as `Paid in full`.
fn stage_name(screen: &Screen) -> String {
    let stage = screen
        .stage
        .strip_prefix(&screen.group)
        .and_then(|s| s.strip_prefix('-'))
        .unwrap_or(&screen.stage);
    capitalised(&stage.replace('-', " "))
}

/// A screen as the viewer reads it from the page.
#[derive(Serialize)]
struct Viewed<'a> {
    title: String,
    test: Option<&'a str>,
    status: Option<&'a str>,
    passed: bool,
    report: Option<&'a str>,
    shapes: Vec<ViewedShape<'a>>,
}

#[derive(Serialize)]
struct ViewedShape<'a> {
    shape: Shape,
    label: &'static str,
    light: Option<&'a Image>,
    dark: Option<&'a Image>,
}

/// The viewer's data as a JSON script block: `<` is escaped so no value can
/// end the block early.
fn viewer_data(screens: &[&Screen]) -> String {
    let viewed: Vec<Viewed> = screens
        .iter()
        .map(|s| Viewed {
            title: format!("{}: {}", group_name(&s.group), stage_name(s)),
            test: s.test.as_deref(),
            status: s.status.as_deref(),
            passed: s.passed(),
            report: s.report.as_deref(),
            shapes: s
                .images
                .iter()
                .map(|(shape, themes)| ViewedShape {
                    shape: *shape,
                    label: shape.label(),
                    light: themes.get(&Theme::Light),
                    dark: themes.get(&Theme::Dark),
                })
                .collect(),
        })
        .collect();
    serde_json::to_string(&viewed)
        .expect("the viewer's data is plain strings and numbers")
        .replace('<', "\\u003c")
}

fn picture(screen: &Screen, shape: Shape, theme: Theme) -> Markup {
    let Some(image) = screen.image(shape, theme) else {
        return html! {};
    };
    let alt = format!(
        "{}, {}, {}",
        stage_name(screen),
        shape.label(),
        theme.name()
    );
    html! {
        img.(theme.name()) loading="lazy" src=(image.thumb) width=(image.w) height=(image.h) alt=(alt);
    }
}

fn frame(screen: &Screen, shape: Shape, class: &str) -> Markup {
    html! {
        span.frame.(class) { (picture(screen, shape, Theme::Light)) (picture(screen, shape, Theme::Dark)) }
    }
}

fn card(index: usize, screen: &Screen) -> Markup {
    let Some(main) = screen.shapes().next() else {
        return html! {};
    };
    let phone = screen.images.contains_key(&Shape::MobilePortrait);
    let full = screen.image(main, Theme::Light).map(|i| i.full.as_str());
    let search = format!(
        "{} {} {}",
        screen.stage,
        screen.test.as_deref().unwrap_or(""),
        group_name(&screen.group)
    )
    .to_lowercase();
    html! {
        a.gcard href=[full] data-screen=(index) data-q=(search) data-phone[phone] {
            (frame(screen, main, "main"))
            @if phone { (frame(screen, Shape::MobilePortrait, "phone")) }
            span.nm { (stage_name(screen)) }
            span.meta {
                @if screen.passed() { span.dot aria-label="passed" {} } @else { span.lowtag { (screen.status.as_deref().unwrap_or("no result")) } }
                (plural(screen.images.len(), "shape", "shapes")) " · " (plural(screen.count, "image", "images"))
                @if !phone { span.nophone { " · no phone shot" } }
            }
        }
    }
}

pub(super) fn page(report: &Report) -> Markup {
    let Some(gallery) = &report.gallery else {
        return html! {};
    };
    let screens: Vec<&Screen> = gallery
        .screens
        .iter()
        .filter(|s| !s.images.is_empty())
        .collect();
    let most_shapes = screens.iter().map(|s| s.images.len()).max().unwrap_or(0);
    let mut groups: BTreeMap<&str, Vec<(usize, &Screen)>> = BTreeMap::new();
    for (i, s) in screens.iter().enumerate() {
        groups.entry(s.group.as_str()).or_default().push((i, s));
    }
    let mut by_size: Vec<(&str, usize)> = groups.iter().map(|(g, s)| (*g, s.len())).collect();
    by_size.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let intro = html! {
        "The browser tests take a picture of " (count(screens.len())) " screens, in light and dark mode and at up to " (count(most_shapes))
        " sizes from phone to desktop: " (count(gallery.total)) " pictures a run. If a change accidentally breaks a layout, it shows up here. Open a screen to compare its sizes and themes."
    };
    html! {
        (heading("Screens", &intro, None))
        div.filters data-filters hidden {
            label.search { input #gq type="search" placeholder="Search screens, e.g. paid, keypad, settings" aria-label="Search screens"; }
            div.seg role="group" aria-label="Device" {
                button type="button" data-device="main" aria-pressed="true" { "Desktop" }
                button type="button" data-device="phone" aria-pressed="false" { "Phone" }
            }
            div.seg role="group" aria-label="Theme" {
                button type="button" data-theme="light" aria-pressed="false" { "Light" }
                button type="button" data-theme="dark" aria-pressed="false" { "Dark" }
                button type="button" data-theme="both" aria-pressed="false" { "Both" }
            }
        }
        div.filters data-filters hidden {
            button.chip type="button" data-group="" aria-pressed="true" { "All screens" }
            @for (group, n) in &by_size {
                button.chip type="button" data-group=(group) aria-pressed="false" { (group_name(group)) " " span.muted { (n) } }
            }
        }
        // `auto` follows the reader's colour scheme until a theme is picked.
        div.gallery #gallery data-device="main" data-theme="auto" {
            @for (group, items) in &groups {
                section.gsec data-group=(group) {
                    h3 { (group_name(group)) " " span { (plural(items.len(), "screen", "screens")) } }
                    div.ggrid { @for (i, s) in items { (card(*i, s)) } }
                }
            }
            p.empty #gnone hidden { "No screens match. Clear the search or pick All screens." }
        }
        dialog.viewer #viewer aria-labelledby="v-title" {}
        script #screens-data type="application/json" { (PreEscaped(viewer_data(&screens))) }
    }
}
