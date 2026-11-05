use proptest::prelude::*;
use std::collections::BTreeSet;

use super::*;

/// What every page must have, for templates that are about something else.
const REQUIRED: &str = "{{status}}{{qr}}{{amount}}{{fiat}}{{address}}";

fn stub(part: &Part) -> String {
    let option = part.option.unwrap_or("");
    format!("[{}{option}]", part.feature.name())
}

fn rendered(source: &str, state: &State) -> String {
    Template::load(source).unwrap().render(state, &mut stub)
}

fn problems(source: &str) -> Vec<String> {
    match check(source) {
        Ok(_) => Vec::new(),
        Err(problems) => problems.into_iter().map(|p| p.message).collect(),
    }
}

fn load_problem(source: &str) -> String {
    Template::load(source).unwrap_err().remove(0).message
}

const PENDING: State = State {
    status: Status::Pending,
    double_spend: false,
    fiat: false,
    refund_locked: false,
};
const PAID: State = State {
    status: Status::Paid,
    ..PENDING
};

#[test]
fn a_template_is_written_back_from_its_tree() {
    assert_eq!(
        rendered(
            "<section class=\"a b\">\n  <p>Pay {{store}} &amp; go</p>{{qr small}}<br>\n</section>",
            &PENDING
        ),
        "<section class=\"a b\">\n  <p>Pay [store] &amp; go</p>[qrsmall]<br>\n</section>"
    );
}

#[test]
fn tokens_take_the_users_form_and_closed_options() {
    for (source, expected) in [
        ("{{qr large}}", "[qrlarge]"),
        ("{{  qr   medium  }}", "[qrmedium]"),
        ("{{amount bare}}", "[amountbare]"),
        ("{{address short}}", "[addressshort]"),
        ("{{amount}}", "[amount]"),
    ] {
        assert_eq!(rendered(source, &PENDING), expected, "{source}");
    }
    assert_eq!(
        load_problem("{{progress huge}}"),
        "{{progress huge}}: \"huge\" isn't an option."
    );
    assert_eq!(
        load_problem("{{qr tiny}}"),
        "{{qr tiny}}: \"tiny\" isn't an option."
    );
    assert_eq!(
        load_problem("{{nope}}"),
        "{{nope}} isn't something a page can show."
    );
    assert_eq!(
        load_problem("{{qr small, large}}"),
        "{{qr}} can take only one option."
    );
}

/// Conditions are AND-only, with `!` and `|`; no else, no nesting.
#[test]
fn conditions_ask_about_the_state() {
    let shown = |condition: &str, state: &State| {
        rendered(&format!("{{{{if {condition}}}}}x{{{{end}}}}"), state).contains('x')
    };
    let unconfirmed = State {
        status: Status::Unconfirmed,
        ..PENDING
    };
    let reversed = State {
        double_spend: true,
        ..PENDING
    };
    let priced = State {
        fiat: true,
        ..PENDING
    };
    let locked = State {
        refund_locked: true,
        ..PENDING
    };
    for (condition, state, expected) in [
        ("awaiting", &PENDING, true),
        ("awaiting", &PAID, false),
        ("!awaiting", &PAID, true),
        ("final", &PAID, true),
        ("final", &unconfirmed, false),
        ("status=paid|overpaid", &PAID, true),
        ("status=paid|overpaid", &PENDING, false),
        ("double_spend", &reversed, true),
        ("!double_spend", &reversed, false),
        ("awaiting, !double_spend", &PENDING, true),
        ("awaiting, !double_spend", &reversed, false),
        ("fiat", &priced, true),
        ("refund_locked", &locked, true),
    ] {
        assert_eq!(shown(condition, state), expected, "{condition}");
    }
    assert_eq!(
        load_problem("{{if a}}{{end}}"),
        "\"a\" isn't something an {{if}} can ask."
    );
    assert_eq!(
        load_problem("{{if status=nope}}{{end}}"),
        "\"nope\" isn't a status."
    );
    assert_eq!(
        load_problem("{{if}}{{end}}"),
        "This {{if}} doesn't ask anything."
    );
    assert_eq!(load_problem("{{else}}"), "{{else}} isn't available.");
    assert_eq!(
        load_problem("{{if fiat}}{{if final}}{{end}}{{end}}"),
        "An {{if}} can't go inside another {{if}}."
    );
    assert_eq!(load_problem("{{if fiat}}"), "This {{if}} has no {{end}}.");
    assert_eq!(load_problem("{{end}}"), "This {{end}} has no {{if}}.");
}

/// A region's `{{if}}` and `{{end}}` share a parent, so what's written back
/// nests the way it reads.
#[test]
fn a_region_stays_inside_one_element() {
    assert_eq!(
        load_problem("{{if fiat}}<div>{{end}}</div>"),
        "This {{end}} is inside a <div> its {{if}} is outside of."
    );
    assert_eq!(
        load_problem("<div>{{if fiat}}</div>{{end}}"),
        "</div> is inside an {{if}} that started outside it."
    );
}

/// A region is always on the page, empty when its condition doesn't hold, so a
/// live update has somewhere to put it.
#[test]
fn regions_render_for_live_updates() {
    let template =
        Template::load("<div>{{if awaiting}}{{qr}}{{end}}{{if final}}<p>Done</p>{{end}}</div>")
            .unwrap();
    assert_eq!(template.regions(), 2);
    assert_eq!(
        template.render(&PENDING, &mut stub),
        "<div><mk-if id=\"mk-if-0\">[qr]</mk-if><mk-if id=\"mk-if-1\"></mk-if></div>"
    );
    assert_eq!(
        template.render_region(1, &PAID, &mut stub).unwrap(),
        "<mk-if id=\"mk-if-1\"><p>Done</p></mk-if>"
    );
    assert_eq!(template.render_region(2, &PAID, &mut stub), None);
    assert_eq!(template.regions_showing(&PENDING), [true, false]);
    assert_eq!(template.regions_showing(&PAID), [false, true]);
    assert_eq!(
        template.parts_showing(&PENDING),
        [Part {
            feature: Feature::Qr,
            option: None
        }]
    );
    assert_eq!(template.parts_showing(&PAID), []);
}

/// Everything a browser would rearrange is refused, so what's written back
/// parses to the tree it was written from.
#[test]
fn what_a_browser_would_rearrange_is_refused() {
    for (source, expected) in [
        (
            "<p><div></div></p>",
            "<div> can't go inside <p>. Browsers would break it apart.",
        ),
        (
            "<p><p></p></p>",
            "<p> can't go inside <p>. Browsers would break it apart.",
        ),
        (
            "<span><section></section></span>",
            "<section> can't go inside <span>. Browsers would break it apart.",
        ),
        (
            "<h1><ul></ul></h1>",
            "<ul> can't go inside <h1>. Browsers would break it apart.",
        ),
        (
            "<p>{{countdown}}</p>",
            "{{countdown}} can't go inside <p>. Browsers would break it apart.",
        ),
        (
            "<p>{{if fiat}}{{qr}}{{end}}</p>",
            "{{qr}} can't go inside <p>. Browsers would break it apart.",
        ),
        ("<li></li>", "<li> must be directly inside <ul> or <ol>."),
        (
            "<ul><div></div></ul>",
            "<div> can't go directly inside a list: put it in an <li>.",
        ),
        (
            "<ul>{{qr}}</ul>",
            "{{qr}} can't go directly inside a list: put it in an <li>.",
        ),
        ("<ul>text</ul>", "Text can't go directly inside <ul>."),
        ("<div>", "<div> is never closed."),
        ("</div>", "</div> has no opening tag."),
        (
            "<div></span></div>",
            "</span> doesn't match <div> on line 1.",
        ),
        ("<div/>", "<div/> can't close itself: write <div></div>."),
        ("<!-- open", "This comment is never closed."),
        ("<", "This < doesn't start a tag."),
        (
            "a & b",
            "This & doesn't start a character this page can use.",
        ),
        (
            "&#x80;",
            "This & doesn't start a character this page can use.",
        ),
        (
            "&#0;",
            "This & doesn't start a character this page can use.",
        ),
        ("\u{1}", "Character U+0001 can't be used."),
    ] {
        assert_eq!(load_problem(source), expected, "{source}");
    }
}

/// Nothing a template can use goes anywhere, loads anything or runs anything.
#[test]
fn what_could_load_or_run_is_refused() {
    for tag in [
        "a", "script", "style", "link", "meta", "iframe", "object", "embed", "base", "img", "svg",
        "video", "audio", "form", "input", "button", "table", "tr", "math", "template", "noscript",
        "mk-if",
    ] {
        assert_eq!(
            load_problem(&format!("<{tag}></{tag}>")),
            format!("<{tag}> can't be used on a checkout page."),
        );
    }
    for (source, expected) in [
        (
            "<div style=\"x\"></div>",
            "The style attribute can't be used.",
        ),
        (
            "<div onclick=\"x\"></div>",
            "The onclick attribute can't be used.",
        ),
        (
            "<div id=\"live-qr\"></div>",
            "The id attribute can't be used.",
        ),
        (
            "<div href=\"x\"></div>",
            "The href attribute can't be used.",
        ),
        (
            "<div data-part=\"qr\"></div>",
            "The data-part attribute can't be used.",
        ),
        ("<div class=x></div>", "class's value needs quotes."),
        ("<div class></div>", "class needs a value in quotes."),
        (
            "<div class=\"a\" class=\"b\"></div>",
            "class is on this tag twice.",
        ),
        (
            "<div class=\"a b;c\"></div>",
            "A class can only use letters, digits, hyphens and underscores.",
        ),
        (
            "<div class=\"{{store}}\"></div>",
            "A {{...}} can't go inside an attribute.",
        ),
        (
            "<div aria-label=\"{{store}}\"></div>",
            "A {{...}} can't go inside an attribute.",
        ),
    ] {
        assert_eq!(load_problem(source), expected, "{source}");
    }
}

#[test]
fn problems_say_which_line() {
    let problems = Template::load("<div>\n\n  <img>\n</div>").unwrap_err();
    assert_eq!(problems[0].place, Place::Template(3));
    assert_eq!(
        problems[0].hint.as_deref(),
        Some("Custom pages load nothing from other sites, so customers on Tor stay private.")
    );
    // CRLF, as a browser's form sends it, counts one line each.
    let problems = Template::load("<div>\r\n<img>\r\n</div>").unwrap_err();
    assert_eq!(problems[0].place, Place::Template(2));
}

/// Several problems at once, so a merchant isn't fixing them one save at a time.
#[test]
fn problems_are_reported_together_and_capped() {
    assert_eq!(
        Template::load("<img>\n{{nope}}\n<a></a>")
            .unwrap_err()
            .len(),
        3
    );
    assert_eq!(
        Template::load(&"<img>".repeat(100)).unwrap_err().len(),
        MAX_PROBLEMS
    );
}

#[test]
fn every_state_must_show_the_status_and_how_to_pay() {
    assert_eq!(problems(REQUIRED), Vec::<String>::new());
    assert_eq!(
        problems("{{qr}}{{amount}}{{fiat}}{{address}}"),
        ["When status=pending, nothing shows {{status}}, so the customer can't tell where their order stands."]
    );
    assert_eq!(
        problems("{{status}}{{amount}}{{fiat}}{{address}}"),
        ["When status=pending, nothing shows {{qr}}, so the customer can't scan a code to pay."]
    );
    assert_eq!(
        problems("{{status}}{{qr}}{{fiat}}{{address}}"),
        ["When status=pending, nothing shows {{amount}}, so the customer can't see how much to pay."]
    );
    assert_eq!(
        problems("{{status}}{{qr}}{{amount}}{{fiat}}"),
        ["When status=pending, nothing shows {{address}}, so the customer can't copy the address to pay."]
    );
    assert_eq!(
        problems("{{status}}{{qr}}{{amount}}{{address}}"),
        ["When status=pending on an order with a fiat price, nothing shows {{fiat}}, so the customer can't see the price they agreed to."]
    );
}

/// `short` and `bare` count, which is how Tiny meets the rule.
#[test]
fn any_form_of_a_required_feature_counts() {
    assert_eq!(
        problems("{{status}}{{qr small}}{{amount bare}}{{fiat}}{{address short}}"),
        Vec::<String>::new()
    );
}

/// Only conditions matter, so showing what's required only some of the time is
/// caught with where it does show.
#[test]
fn a_condition_that_hides_something_required_says_where() {
    let source = "{{status}}{{amount}}{{fiat}}{{address}}\n{{if status=pending}}{{qr}}{{end}}";
    let found = check(source).unwrap_err();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].message,
        "When status=partial, nothing shows {{qr}}, so the customer can't scan a code to pay."
    );
    assert_eq!(
        found[0].hint.as_deref(),
        Some("Line 2 only shows it when {{if status=pending}}.")
    );
    assert_eq!(found[0].place, Place::EveryState);
    // Once paid, none of it is needed.
    assert_eq!(
        problems("{{status}}{{if awaiting}}{{qr}}{{amount}}{{fiat}}{{address}}{{end}}"),
        Vec::<String>::new()
    );
}

#[test]
fn a_live_feature_can_only_show_once_in_a_state() {
    assert_eq!(
        problems(&format!("{REQUIRED}\n{{{{qr}}}}")),
        ["{{qr}} shows twice when status=pending, on lines 1 and 2. It can only show once at a time."]
    );
    // In different states it's fine; and the store's name isn't live.
    assert_eq!(
        problems(
            "{{status}}{{amount}}{{fiat}}{{address}}{{store}}{{store}}\n{{if awaiting}}{{qr}}{{end}}\n{{if !awaiting}}{{qr}}{{end}}"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn there_are_sixty_four_states_and_each_status_is_among_them() {
    let states: Vec<State> = all_states().collect();
    assert_eq!(states.len(), 64);
    for status in Status::ALL {
        assert!(states.iter().any(|state| state.status == status));
    }
}

#[test]
fn size_counts_text_features_and_conditions() {
    let size = |source: &str| Template::load(source).unwrap().size();
    // One unit per 32 bytes of text.
    assert_eq!(size(""), 0);
    assert_eq!(size("a"), 1);
    assert_eq!(size(&"a".repeat(32)), 1);
    assert_eq!(size(&"a".repeat(33)), 2);
    // A feature by what it writes.
    assert_eq!(size("{{store}}"), 2);
    assert_eq!(size("{{qr}}"), 40);
    // A condition: 3, and 1 per thing it asks; both branches count.
    assert_eq!(size("{{if fiat}}{{end}}"), 4);
    assert_eq!(size("{{if fiat, final}}{{end}}"), 5);
    assert_eq!(
        size("{{if fiat}}{{qr}}{{end}}{{if !fiat}}{{qr}}{{end}}"),
        4 + 40 + 4 + 40
    );
    // An element, and its attributes' text.
    assert_eq!(size("<div></div>"), 1);
    assert_eq!(size("<div class=\"a\"></div>"), 2);
}

#[test]
fn a_template_over_budget_is_refused_at_save_and_loads_once_published() {
    let padding = "{{if fiat}}{{qr}}{{end}}".repeat(100);
    let source = format!("{REQUIRED}{padding}");
    assert!(source.len() < MAX_SOURCE_BYTES, "{}", source.len());
    let size = Template::load(&source).unwrap().size();
    assert!(size > BUDGET, "{size}");
    let found = check(&source).unwrap_err();
    assert_eq!(
        found[0].message,
        format!("This template is {} of 4,000.", format_size(size))
    );
    assert_eq!(
        found[0].hint.as_deref(),
        Some("Remove some text or conditions.")
    );
}

/// A template at its budget costs little: what it writes is bounded by its
/// size, whichever way it's spent.
#[test]
fn a_template_at_its_budget_writes_a_bounded_page() {
    let template = check(&format!("{REQUIRED}{}", "{{store}}".repeat(1960))).unwrap();
    assert!(template.size() > BUDGET - 100, "{}", template.size());
    let html = template.render(&PENDING, &mut stub);
    assert!(html.len() < 32 * 1024, "{}", html.len());
}

#[test]
fn the_cheap_limits_come_before_reading_it() {
    let huge = "a".repeat(MAX_SOURCE_BYTES + 1);
    assert_eq!(
        load_problem(&huge),
        "This template is over the limit of 32 KiB."
    );
    let tags = "<".repeat(MAX_TAGS + 1);
    assert_eq!(
        load_problem(&tags),
        "This template has more than 2000 tags."
    );
    // Deep nesting stops at its limit, on an explicit stack.
    let deep = "<div>".repeat(MAX_DEPTH + 1);
    assert!(
        load_problem(&deep).contains("is never closed")
            || Template::load(&deep)
                .unwrap_err()
                .iter()
                .any(|problem| problem.message == "Elements can only nest 32 deep.")
    );
    assert!(Template::load(&deep)
        .unwrap_err()
        .iter()
        .any(|problem| problem.message == "Elements can only nest 32 deep."));
}

#[test]
fn comments_are_dropped_and_their_tokens_ignored() {
    assert_eq!(rendered("a<!-- {{nope}} <img> -->b", &PENDING), "ab");
}

#[test]
fn character_references_are_decoded_and_written_back_escaped() {
    assert_eq!(
        rendered(
            "&lt;script&gt; &amp; &quot;x&quot; &apos;y&apos; &#65;&#x42;",
            &PENDING
        ),
        "&lt;script&gt; &amp; \"x\" 'y' AB"
    );
    assert_eq!(rendered("a&nbsp;b", &PENDING), "a&nbsp;b");
    assert_eq!(
        rendered("<p aria-label=\"a &amp; &quot;b&quot;\"></p>", &PENDING),
        "<p aria-label=\"a &amp; &quot;b&quot;\"></p>"
    );
    // What looks like a token, spelled out, is text.
    assert_eq!(rendered("&#123;&#123;qr}}", &PENDING), "{{qr}}");
}

/// Published templates must keep working in every later release. Add a line
/// here for anything new; never change or remove one. Run by `load`, as a
/// published template is: the budget and every-state checks are for saving.
#[test]
fn every_form_published_templates_can_use_still_loads() {
    let corpus = [
        "{{store}}{{amount}}{{amount bare}}{{fiat}}{{order-id}}",
        "{{qr}}{{qr small}}{{qr medium}}{{qr large}}",
        "{{address}}{{address short}}{{status}}{{countdown}}{{progress}}{{refund}}{{payments}}",
        "{{if awaiting}}{{end}}{{if final}}{{end}}{{if double_spend}}{{end}}{{if fiat}}{{end}}{{if refund_locked}}{{end}}",
        "{{if !awaiting, !final, !double_spend, !fiat, !refund_locked}}{{end}}",
        "{{if status=pending|partial|unconfirmed|confirming|paid|overpaid|expired|cancelled}}{{end}}",
        "<section><div><article><aside><header></header><footer></footer><main></main><nav></nav></aside></article></div></section>",
        "<figure><figcaption></figcaption></figure><blockquote></blockquote><hr><ul><li></li></ul><ol><li></li></ol>",
        "<p><b></b><br><code></code><em></em><i></i><small></small><span></span><strong></strong></p>",
        "<h1></h1><h2></h2><h3></h3><h4></h4><h5></h5><h6></h6>",
        "<div class=\"a mk-row\" lang=\"en\" aria-label=\"x\"></div>",
    ];
    for source in corpus {
        assert!(Template::load(source).is_ok(), "{source}");
    }
}

#[test]
fn the_version_token_changes_with_any_of_its_parts_and_not_with_line_endings() {
    let token = version_token("store", "<p>a\n</p>", "p{}");
    assert_eq!(token.len(), 32);
    assert!(token
        .bytes()
        .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)));
    assert_eq!(token, version_token("store", "<p>a\r\n</p>", "p{}"));
    assert_ne!(token, version_token("other", "<p>a\n</p>", "p{}"));
    assert_ne!(token, version_token("store", "<p>b\n</p>", "p{}"));
    assert_ne!(token, version_token("store", "<p>a\n</p>", "a{}"));
    // Moving text between the template and the CSS isn't the same version.
    assert_ne!(version_token("s", "ab", "c"), version_token("s", "a", "bc"));
    assert_ne!(version_token("sa", "b", "c"), version_token("s", "ab", "c"));
}

#[test]
fn sizes_read_as_the_page_shows_them() {
    for (size, expected) in [
        (0, "0"),
        (412, "412"),
        (4000, "4,000"),
        (1234567, "1,234,567"),
    ] {
        assert_eq!(format_size(size), expected);
    }
}

// The CSS.

fn classes(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn sanitized(css: &str) -> String {
    css::sanitize(css, &classes(&["pay", "note"])).unwrap()
}

fn css_problem(css: &str) -> String {
    css::sanitize(css, &classes(&["pay"]))
        .unwrap_err()
        .remove(0)
        .message
}

#[test]
fn css_is_scoped_to_the_page_and_written_from_its_tokens() {
    for (css, expected) in [
        ("h1{color:red}", "#checkout-root h1{color:red;}"),
        (
            "h1 { color : red ; } /* note */",
            "#checkout-root h1{color:red;}",
        ),
        (
            ".pay,h1>em{margin:1px 2px}",
            "#checkout-root .pay,#checkout-root h1>em{margin:1px 2px;}",
        ),
        (".mk-row{gap:1em}", "#checkout-root .mk-row{gap:1em;}"),
        (
            "[data-part=\"qr\"]{flex:none}",
            "#checkout-root [data-part=\"qr\"]{flex:none;}",
        ),
        (
            "[data-part=qr]{flex:none}",
            "#checkout-root [data-part=\"qr\"]{flex:none;}",
        ),
        (
            "h1::before{content:\"x\"}",
            "#checkout-root h1::before{content:\"x\";}",
        ),
        ("p:hover{color:red}", "#checkout-root p:hover{color:red;}"),
        (
            "p:not(.pay){color:red}",
            "#checkout-root p:not(.pay){color:red;}",
        ),
        (
            "p:nth-child(2n+1){color:red}",
            "#checkout-root p:nth-child(2n/**/+1){color:red;}",
        ),
        (
            "h1{color:red!important}",
            "#checkout-root h1{color:red!important;}",
        ),
        (
            "h1{margin:calc(1px + var(--x))}",
            "#checkout-root h1{margin:calc(1px + var(--x));}",
        ),
        (
            "h1{background:linear-gradient(red,blue)}",
            "#checkout-root h1{background:linear-gradient(red,blue);}",
        ),
        ("h1{--mine:1px}", "#checkout-root h1{--mine:1px;}"),
    ] {
        assert_eq!(sanitized(css), expected, "{css}");
    }
}

#[test]
fn at_rules_that_group_stay_and_their_rules_are_scoped() {
    assert_eq!(
        sanitized("@media (max-width: 480px) { .pay { flex-direction: column; } }"),
        "@media (max-width: 480px){#checkout-root .pay{flex-direction:column;}}"
    );
    assert_eq!(
        sanitized("@supports (display: grid) { h1 { display: grid } }"),
        "@supports (display: grid){#checkout-root h1{display:grid;}}"
    );
    assert_eq!(
        sanitized("@keyframes pop { from { opacity: 0 } 50%, to { opacity: 1 } }"),
        "@keyframes pop{from{opacity:0;}50%,to{opacity:1;}}"
    );
}

/// However it's spelled: `cssparser` undoes escapes, as a browser does.
#[test]
fn what_loads_from_elsewhere_is_refused_however_it_is_escaped() {
    let url = "url() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private.";
    for css in [
        "h1{background:url(https://x.example/a.png)}",
        "h1{background:URL(https://x.example/a.png)}",
        "h1{background:url( 'https://x.example/a.png' )}",
        "h1{background:u\\72l(https://x.example/a.png)}",
        "h1{background:\\75\\72\\6c(https://x.example/a.png)}",
        "h1{background:linear-gradient(red,url(x))}",
        "h1{--x:url(x)}",
    ] {
        assert_eq!(css_problem(css), url, "{css}");
    }
    for (css, expected) in [
        ("h1{background:image-set(\"x\" 1x)}", "image-set() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private."),
        ("h1{background:-webkit-image-set(\"x\" 1x)}", "-webkit-image-set() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private."),
        ("h1{src:src(\"x\")}", "src() can't be used. Custom pages load nothing from other sites, so customers on Tor stay private."),
        ("@import \"x\";", "@import can't be used."),
        ("@IMPORT url(x);", "@IMPORT can't be used."),
        ("@im\\70 ort \"x\";", "@import can't be used."),
        ("@font-face{font-family:x;src:url(x)}", "@font-face can't be used."),
        ("@namespace x;", "@namespace can't be used."),
        ("@layer x{}", "@layer can't be used."),
        ("@charset \"x\";", "@charset can't be used."),
        ("@page{}", "@page can't be used."),
        ("h1{behavior:x}", "behavior can't be set."),
        ("h1{-moz-binding:x}", "-moz-binding can't be set."),
    ] {
        assert_eq!(css_problem(css), expected, "{css}");
    }
}

#[test]
fn selectors_reach_only_what_a_page_can_rely_on() {
    for (css, expected) in [
        (".nope{}", ".nope isn't a class in your template."),
        (
            ".pay .pay-wrap{}",
            ".pay-wrap isn't a class in your template.",
        ),
        (
            "#checkout-root{}",
            "#checkout-root can't be selected: ids are the page's own.",
        ),
        (
            "#live-qr{}",
            "#live-qr can't be selected: ids are the page's own.",
        ),
        (
            "html{}",
            "html can't be selected: everything is inside your page already.",
        ),
        (
            "BODY{}",
            "BODY can't be selected: everything is inside your page already.",
        ),
        (
            ":root{}",
            ":root can't be selected: everything is inside your page already.",
        ),
        (
            "[data-order-id]{}",
            "Only [data-part=\"...\"] can be selected, by its exact value.",
        ),
        (
            "[data-part^=\"q\"]{}",
            "Only [data-part=\"...\"] can be selected, by its exact value.",
        ),
        (
            "[data-status=paid]{}",
            "Only [data-part=\"...\"] can be selected, by its exact value.",
        ),
        (
            "input[value^=\"4\"]{}",
            "Only [data-part=\"...\"] can be selected, by its exact value.",
        ),
        ("[data-part=\"nope\"]{}", "\"nope\" isn't a data-part."),
        (":not(.nope){}", ".nope isn't a class in your template."),
        (":is(#x){}", "#x can't be selected: ids are the page's own."),
    ] {
        assert_eq!(css_problem(css), expected, "{css}");
    }
}

#[test]
fn what_keeps_a_page_payable_cant_be_overridden_from_css() {
    for (css, expected) in [
        ("h1{--qr-bg:red}", "--qr-bg can't be set."),
        (
            "h1{--state-paid-border:red}",
            "--state-paid-border can't be set.",
        ),
    ] {
        assert_eq!(css_problem(css), expected, "{css}");
    }
    assert_eq!(
        css::sanitize("h1{--qr-bg:red}", &classes(&[])).unwrap_err()[0]
            .hint
            .as_deref(),
        Some("The code stays dark on white, so wallets can scan it.")
    );
}

#[test]
fn css_that_cant_be_read_is_said_so_with_its_line() {
    let found = css::sanitize("h1{}\n\n.nope{}", &classes(&[])).unwrap_err();
    assert_eq!(found[0].place, Place::Css(3));
    for (css, expected) in [
        ("h1", "This rule has no { }."),
        ("h1;", "This rule has no { }."),
        ("{color:red}", "A selector is missing here."),
        (
            "h1{ p { color:red } }",
            "p needs a : here. A rule can't go inside a rule.",
        ),
        ("@media (max-width:1px)", "This rule has no { }."),
    ] {
        assert_eq!(css_problem(css), expected, "{css}");
    }
    assert_eq!(
        css::sanitize(&"a".repeat(css::MAX_CSS_BYTES + 1), &classes(&[])).unwrap_err()[0].message,
        "This CSS is over the limit of 32 KiB."
    );
    let deep = format!("h1{{margin:{}}}", "(".repeat(100));
    assert_eq!(css_problem(&deep), "This CSS nests too deeply.");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any text gives a template or problems.
    #[test]
    fn any_text_is_read_without_a_panic(source in "\\PC{0,400}") {
        let _ = check(&source);
    }

    /// Template-shaped noise reaches further in than random text does, and
    /// what's written back reads as the same template.
    #[test]
    fn what_is_written_back_reads_the_same(
        pieces in proptest::collection::vec(prop_oneof![
            Just("<div>"), Just("</div>"), Just("<p>"), Just("</p>"), Just("<ul>"), Just("</ul>"),
            Just("<li>"), Just("</li>"), Just("<br>"), Just("text"), Just("&amp;"), Just("{{qr}}"),
            Just("{{store}}"), Just("<div class=\"a\">"), Just("<span>"), Just("</span>"), Just("&lt;"),
        ], 0..30)
    ) {
        let source = pieces.concat();
        if let Ok(template) = Template::load(&source) {
            let mut keep = |part: &Part| format!("{{{{{}}}}}", part.feature.name());
            let written = template.render(&PENDING, &mut keep);
            let again = Template::load(&written).expect("what's written back reads");
            prop_assert_eq!(again.render(&PENDING, &mut keep), written);
        }
    }

    /// Any text as CSS gives CSS or problems, and what's accepted stays in the
    /// page, however it was spelled.
    #[test]
    fn any_css_is_read_without_a_panic_and_stays_in_the_page(source in "\\PC{0,400}") {
        if let Ok(written) = css::sanitize(&source, &classes(&["pay"])) {
            prop_assert!(css::written_is_safe(&written), "{}", written);
        }
    }

    #[test]
    fn written_css_reads_back_the_same(
        pieces in proptest::collection::vec(prop_oneof![
            Just("h1"), Just(".pay"), Just("{"), Just("}"), Just(":"), Just(";"), Just("color"),
            Just("red"), Just(" "), Just("("), Just(")"), Just(","), Just("@media "), Just("1px"),
            Just("--x"), Just("\"s\""), Just("/*c*/"), Just("\\72"), Just("calc("), Just("-"), Just("+"),
            Just("\\ "), Just("\\\n"), Just("~"), Just("&"), Just(">"), Just(":"), Just("*"),
        ], 0..30)
    ) {
        let source = pieces.concat();
        if let Ok(first) = css::sanitize(&source, &classes(&["pay"])) {
            prop_assert!(css::written_is_safe(&first), "{}", first);
            let unscoped = first.replace("#checkout-root ", "");
            if let Ok(second) = css::sanitize(&unscoped, &classes(&["pay"])) {
                prop_assert_eq!(first, second);
            }
        }
    }
}

/// Each of these got out of the page, or crashed the process, before review.
#[test]
fn what_escaped_scoping_is_refused() {
    let refused = |css: &str| {
        let result = css::sanitize(css, &classes(&["pay"]));
        assert!(result.is_err(), "accepted {css}: {result:?}");
    };
    for css in [
        // A trailing escaped space, left to escape the `{` once trimmed.
        "h1\\ {--x:{}html{background:red}}",
        "h1\\ {--x:{}@import \"/evil.css\";}",
        "h1\\\n{}",
        // `{}` in a value is a nested rule to a browser.
        "h1{body:not(&){background:red}}",
        "h1{--x:{}}",
        "h1{section:not(&){--qr-bg:red}}",
        "@media ({}){h1{}}",
        // A leading combinator reaches the scope's siblings.
        "~ *{background:red}",
        "+ *{background:red}",
        "> *{background:red}",
        "h1, ~ *{background:red}",
        "&{background:red}",
        "h1 &{background:red}",
        ":is(&){background:red}",
        // A stray backslash.
        "h1{color:a\\\n;}",
    ] {
        refused(css);
    }
    // Written as what it is: an ident with a space in it, still before its `{`.
    let written = sanitized("h1\\ {color:red}");
    assert_eq!(written, "#checkout-root h1\\ {color:red;}");
    assert!(css::written_is_safe(&written));
    let written = sanitized("h1{color:a\\ ;background:red}");
    assert_eq!(written, "#checkout-root h1{color:a\\ ;background:red;}");
}

/// A long run of colons recursed for each one, past the stack a server thread
/// has.
#[test]
fn a_run_of_colons_cant_overflow_the_stack() {
    let colons = format!("{}{{}}", ":".repeat(32_000));
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || assert!(css::sanitize(&colons, &classes(&[])).is_err()))
        .unwrap()
        .join()
        .unwrap();
}

/// The check on what's written doesn't take the writer's word for anything.
#[test]
fn the_check_on_written_css_refuses_what_gets_out() {
    for written in [
        "h1{color:red;}",
        "html{background:red;}",
        "#checkout-root ~ *{}",
        "#checkout-root h1,html{}",
        "#checkout-root{}",
        "#checkout-root h1{--x:{};}",
        "#checkout-root h1{body:not(&){color:red}}",
        "#checkout-root h1{background:url(x);}",
        "#checkout-root h1{background:image-set(\"x\" 1x);}",
        "@import \"x\";",
        "@font-face{src:url(x)}",
        "@layer a{}",
        "@media ({}){#checkout-root h1{}}",
        "@media print{h1{}}",
        "#checkout-root h1\\{--x:{}html{color:red};}",
    ] {
        assert!(!css::written_is_safe(written), "{written}");
    }
    for written in [
        "",
        "#checkout-root h1{color:red;}",
        "#checkout-root .a,#checkout-root h1>em{margin:1px;}",
        "@media (max-width: 1px){#checkout-root h1{color:red;}}",
        "@keyframes pop{from{opacity:0;}50%,to{opacity:1;}}",
        "#checkout-root [data-part=\"qr\"]{flex:none;}",
        "#checkout-root h1::before{content:\"url(x)\";}",
    ] {
        assert!(css::written_is_safe(written), "{written}");
    }
}

/// Bits a stricter reading found: none reached a browser differently, and each
/// is now refused or counted.
#[test]
fn the_parser_reads_what_it_says_it_does() {
    assert_eq!(
        load_problem("<ul>&nbsp;</ul>"),
        "Text can't go directly inside <ul>."
    );
    assert_eq!(
        load_problem("&#+65;"),
        "This & doesn't start a character this page can use."
    );
    assert_eq!(
        load_problem("&#x+41;"),
        "This & doesn't start a character this page can use."
    );
    assert_eq!(load_problem("{{\nqr}}"), "A {{...}} must be on one line.");
    assert_eq!(
        load_problem("<div aria-label=\"&#123;&#123;store}}\"></div>"),
        "A {{...}} can't go inside an attribute."
    );
    assert_eq!(
        Template::load("{{if status=paid|overpaid|expired}}{{end}}")
            .unwrap()
            .size(),
        6
    );
}

/// What the browser test of CSS reads: see
/// `e2e/browser/tests/checkout-template-corpus.spec.js`.
#[test]
fn the_browser_css_corpus_is_what_this_crate_writes() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../e2e/browser/fixtures/css-corpus.json"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut corpus: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    let rewrite = std::env::var_os("CORPUS_REWRITE").is_some();
    for entry in &mut corpus {
        let source = entry["css"].as_str().unwrap().to_string();
        let written = css::sanitize(&source, &classes(&["pay"]))
            .unwrap_or_else(|e| panic!("{source}: {e:?}"));
        if rewrite {
            entry["written"] = serde_json::Value::String(written);
        } else {
            assert_eq!(entry["written"].as_str().unwrap(), written, "{source}");
        }
    }
    if rewrite {
        std::fs::write(path, serde_json::to_string_pretty(&corpus).unwrap() + "\n").unwrap();
    }
}

/// What the browser test reads (`e2e/browser/tests/checkout-template-corpus.spec.js`):
/// what this crate writes, for templates chosen to tempt a browser into
/// rearranging them. If it builds anything but the tree written, the
/// checkout's sanitising can be got around. `CORPUS_REWRITE=1` writes the new
/// answers after a deliberate change.
#[test]
fn the_browser_corpus_is_what_this_crate_writes() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../e2e/browser/fixtures/template-corpus.json"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut corpus: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(corpus.len() >= 40, "{}", corpus.len());
    let mut stub =
        |part: &Part| format!("<mk-part data-part=\"{}\"></mk-part>", part.feature.name());
    let rewrite = std::env::var_os("CORPUS_REWRITE").is_some();
    for entry in &mut corpus {
        let source = entry["source"].as_str().unwrap().to_string();
        let template = Template::load(&source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        let written = template.render(&PENDING, &mut stub);
        if rewrite {
            entry["html"] = serde_json::Value::String(written);
        } else {
            assert_eq!(entry["html"].as_str().unwrap(), written, "{source}");
        }
    }
    if rewrite {
        std::fs::write(path, serde_json::to_string_pretty(&corpus).unwrap() + "\n").unwrap();
    }
}
