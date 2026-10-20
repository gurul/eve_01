//! Port of `bridge/tests/test_telegram_format.py`: one shape for everything eve sends: escaped, titled,
//! in paragraphs, split without breaking a tag, and each message kind rendered from a realistic sample.
//! Plus a differential test against fixtures the Python module itself produced.

use eve_telegram_format::{
    compose, escape, inline, plain, render_body, split, table_blocks, visible, MAX_MESSAGE_CHARS, MAX_TITLE_CHARS,
    PARSE_MODE,
};
use regex::Regex;

/// Every tag opened in a piece is closed in it, in order.
fn balanced(piece: &str) -> bool {
    let tag = Regex::new(r"<(/?)([a-z]+)[^>]*>").unwrap();
    let mut stack: Vec<String> = Vec::new();
    for m in tag.captures_iter(piece) {
        if !m[1].is_empty() {
            if stack.last().map(String::as_str) != Some(&m[2]) {
                return false;
            }
            stack.pop();
        } else {
            stack.push(m[2].to_string());
        }
    }
    stack.is_empty()
}

fn len(s: &str) -> usize {
    s.chars().count()
}

// ---- escaping ----------------------------------------------------------------------------------

#[test]
fn test_content_is_escaped_and_only_the_three_reserved_characters_change() {
    assert_eq!(escape("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    assert_eq!(escape("quotes \"fine\", café, 日本語, 3 + 4 = 7"), "quotes \"fine\", café, 日本語, 3 + 4 = 7");
    // something that looks like markup in a model's answer is shown, never interpreted
    assert_eq!(
        render_body("use <b>real</b> tags & <script>"),
        "use &lt;b&gt;real&lt;/b&gt; tags &amp; &lt;script&gt;"
    );
    assert_eq!(visible(&render_body("a < b & c > d")), "a < b & c > d");
    assert_eq!(PARSE_MODE, "HTML");
}

#[test]
fn test_a_title_is_escaped_clipped_and_stripped_of_emoji_and_dashes() {
    assert_eq!(
        compose("body", Some("Task <done> & dusted \u{1F389}"), None),
        "<b>Task &lt;done&gt; &amp; dusted</b>\n\nbody"
    );
    assert_eq!(compose("body", Some("Claude \u{2014} eve"), None), "<b>Claude, eve</b>\n\nbody");
    let long_title = "open the calculator and add up every receipt on the desk, then ".repeat(3);
    let composed = compose("body", Some(&long_title), None);
    let head = composed.split('\n').next().unwrap();
    assert!(head.starts_with("<b>open the calculator") && head.ends_with("\u{2026}</b>"));
    assert!(len(&visible(head)) <= MAX_TITLE_CHARS);
    // no title: the body alone
    assert_eq!(compose("body", None, None), "body");
    assert_eq!(compose("body", Some(""), None), "body");
    // no body: the title alone
    assert_eq!(compose("", Some("Only a title"), None), "<b>Only a title</b>");
}

// ---- markdown to Telegram HTML -----------------------------------------------------------------

#[test]
fn test_markdown_from_a_model_becomes_telegram_markup_not_raw_stars_and_hashes() {
    let text = concat!(
        "## What I did\n",
        "I fixed the **flaky test** in `tests/test_x.py` \u{2014} it raced on a *timer*.\n",
        "\n",
        "- Added a lock around `_flush`\n",
        "* Removed the __sleep__ hack\n",
        "1. First\n",
        "2) Second\n",
        "\n",
        "> a quote\n",
        "---\n",
        "See [the docs](https://example.com/a?b=1&c=2) or ~~the old page~~."
    );
    let out = render_body(text);
    assert_eq!(
        out,
        concat!(
            "<b>What I did</b>\n",
            "I fixed the <b>flaky test</b> in <code>tests/test_x.py</code>, it raced on a <i>timer</i>.\n",
            "\n",
            "\u{2022} Added a lock around <code>_flush</code>\n",
            "\u{2022} Removed the <b>sleep</b> hack\n",
            "1. First\n",
            "2. Second\n",
            "\n",
            "<i>a quote</i>\n",
            "\n",
            "See <a href=\"https://example.com/a?b=1&amp;c=2\">the docs</a> or <s>the old page</s>."
        )
    );
    let shown = visible(&out);
    assert!(!shown.contains('*') && !shown.contains('#') && !shown.contains('`'));
}

#[test]
fn test_a_link_url_keeps_its_underscores_and_stars() {
    // The styles ran over the finished href: https://x.com/_foo_/bar became href="https://x.com/<i>foo</i>/bar",
    // which Telegram rejects as HTML
    assert_eq!(inline("[docs](https://x.com/_foo_/bar)"), "<a href=\"https://x.com/_foo_/bar\">docs</a>");
    assert_eq!(
        inline("see [a](https://x.com/**b**/c) and **bold**"),
        "see <a href=\"https://x.com/**b**/c\">a</a> and <b>bold</b>"
    );
    assert_eq!(inline("[_the_ docs](https://x.com/~~y~~)"), "<a href=\"https://x.com/~~y~~\"><i>the</i> docs</a>");
}

#[test]
fn test_code_fences_become_pre_blocks_verbatim_and_escaped() {
    let text = "Run this:\n```python\nif a < b and c > d:\n    print('x')   # two  spaces kept\n```\nThen `pytest -q`.";
    assert_eq!(
        render_body(text),
        concat!(
            "Run this:\n\n",
            "<pre>if a &lt; b and c &gt; d:\n    print('x')   # two  spaces kept</pre>\n\n",
            "Then <code>pytest -q</code>."
        )
    );
    // an unclosed fence is still shown as code; markdown inside code is not styled
    assert_eq!(render_body("```\n**not bold** _nor italic_"), "<pre>**not bold** _nor italic_</pre>");
    assert_eq!(render_body("`**kept**`"), "<code>**kept**</code>");
}

#[test]
fn test_underscores_and_stars_inside_words_are_left_alone() {
    assert_eq!(render_body("snake_case_name and file_name.py"), "snake_case_name and file_name.py");
    assert_eq!(render_body("2*3*4 = 24"), "2*3*4 = 24");
    assert_eq!(render_body("a * b"), "a * b");
    assert_eq!(render_body("_italic_ and *this*"), "<i>italic</i> and <i>this</i>");
}

#[test]
fn test_a_wide_markdown_table_becomes_a_block_per_row_with_header_value_lines() {
    // Codex reported tonight's showtimes as a pipe table (owner, 2026-09-24: "Format is bad"): Telegram has no
    // tables, so each row is its first cell in bold and a "header: value" line per filled cell after it
    let text = concat!(
        "All times below are **PM**.\n\n",
        "| Movie | Regal Thornton Place | AMC Oak Tree 6 |\n",
        "|---|---|---|\n",
        "| Avengers Endgame: Encore | 6:10 | 6:45; 8:45 (3D) |\n",
        "| Practical Magic 2 | 9:50 | |\n\n",
        "Regal Thornton Place: 316 NE Thornton Place."
    );
    assert_eq!(
        render_body(text),
        concat!(
            "All times below are <b>PM</b>.\n\n",
            "<b>Avengers Endgame: Encore</b>\n",
            "Regal Thornton Place: 6:10\n",
            "AMC Oak Tree 6: 6:45; 8:45 (3D)\n\n",
            "<b>Practical Magic 2</b>\n",
            "Regal Thornton Place: 9:50\n\n",
            "Regal Thornton Place: 316 NE Thornton Place."
        )
    );
    assert!(!visible(&render_body(text)).contains('|'));
}

#[test]
fn test_a_two_column_table_is_one_line_per_row_and_cells_keep_their_styles() {
    let text = concat!(
        "| Setting | Default |\n",
        "|:--|--:|\n",
        "| `EVE_X` | 0 |\n",
        "| **Y** | *off* \u{2014} for now |\n",
        "| | |\n",
        "| Z | |"
    );
    assert_eq!(render_body(text), "<b><code>EVE_X</code></b>: 0\n<b>Y</b>: <i>off</i>, for now\n<b>Z</b>");
    // a lone pipe line without a separator under it is prose, not a table
    assert_eq!(render_body("| just a line |\nnext"), "| just a line |\nnext");
    // a table right after a fence and one ending the text both close cleanly
    assert_eq!(render_body("```\nx | y\n```\n| a | b |\n|-|-|\n| 1 | 2 |"), "<pre>x | y</pre>\n\n<b>1</b>: 2");
}

#[test]
fn test_paragraphs_are_kept_blank_runs_collapsed_and_prose_is_plain() {
    let text = "First line  \nsecond line\n\n\n\n\nnew paragraph \u{2014} with a dash \u{1F680}\r\nwindows line";
    assert_eq!(render_body(text), "First line\nsecond line\n\nnew paragraph, with a dash\nwindows line");
    assert_eq!(render_body(""), "");
    assert_eq!(render_body("\n\n  \n"), "");
}

// ---- splitting ---------------------------------------------------------------------------------

#[test]
fn test_short_messages_are_one_piece_and_empty_ones_none() {
    assert_eq!(split("hi", MAX_MESSAGE_CHARS), vec!["hi"]);
    assert!(split("", MAX_MESSAGE_CHARS).is_empty());
    assert!(split("  \n ", MAX_MESSAGE_CHARS).is_empty());
    let full = "a".repeat(MAX_MESSAGE_CHARS);
    assert_eq!(split(&full, MAX_MESSAGE_CHARS), vec![full.clone()]);
}

#[test]
fn test_a_long_message_is_cut_on_paragraph_boundaries_with_the_title_on_the_first_piece() {
    let body: Vec<String> = (0..30).map(|i| format!("Paragraph {i}. {}", "word ".repeat(100))).collect();
    let message = compose(&body.join("\n\n"), Some("Task done"), Some("the goal"));
    let pieces = split(&message, MAX_MESSAGE_CHARS);
    assert!(pieces.len() > 1 && pieces.iter().all(|p| len(p) <= MAX_MESSAGE_CHARS));
    assert!(pieces[0].starts_with("<b>Task done</b>\n<i>the goal</i>\n\nParagraph 0."));
    assert!(pieces[1..].iter().all(|p| p.starts_with("Paragraph ")));
    assert!(pieces.iter().all(|p| balanced(p)));
    assert_eq!(pieces.join("\n\n"), message); // nothing lost, nothing added
}

#[test]
fn test_a_paragraph_longer_than_the_limit_is_cut_at_a_line_or_a_space_with_tags_rebalanced() {
    // one <pre> block of 8000 characters: cut at line ends, closed at each cut, reopened after it
    let rows: Vec<String> = (0..120).map(|i| format!("row {i:03} {}", "y".repeat(60))).collect();
    let rows = rows.join("\n");
    let message = compose(&format!("```\n{rows}\n```"), Some("Claude"), None);
    let pieces = split(&message, MAX_MESSAGE_CHARS);
    assert!(pieces.len() >= 3 && pieces.iter().all(|p| len(p) <= MAX_MESSAGE_CHARS));
    assert!(pieces.iter().all(|p| balanced(p)));
    assert!(pieces[0].starts_with("<b>Claude</b>\n\n<pre>row 000 ") && pieces[0].ends_with("</pre>"));
    assert!(pieces[1..].iter().all(|p| p.starts_with("<pre>row ") && p.ends_with("</pre>")));
    let shown: String = pieces
        .iter()
        .map(|p| {
            let v = visible(p);
            v.strip_prefix("Claude\n\n").map(str::to_string).unwrap_or(v)
        })
        .collect();
    assert_eq!(shown.replace('\n', ""), rows.replace('\n', "")); // every character of every row arrives

    // bold that spans a cut, in one paragraph with no newline at all
    let words = format!("<b>{}</b>", "word ".repeat(2000));
    let pieces = split(&words, MAX_MESSAGE_CHARS);
    assert!(pieces.len() >= 3 && pieces.iter().all(|p| len(p) <= MAX_MESSAGE_CHARS));
    assert!(pieces.iter().all(|p| balanced(p) && p.starts_with("<b>") && p.ends_with("</b>")));
    let shown: Vec<String> = pieces.iter().map(|p| visible(p)).collect();
    let joined = shown.join(" ");
    let all: Vec<&str> = joined.split_whitespace().collect(); // pieces are separate messages
    assert_eq!(all, vec!["word"; 2000]);

    // a link's attributes survive the reopening
    let link = format!("<a href=\"https://example.com/x\">{}</a>", "x".repeat(6000));
    let pieces = split(&link, MAX_MESSAGE_CHARS);
    assert!(pieces.iter().all(|p| p.starts_with("<a href=\"https://example.com/x\">") && p.ends_with("</a>")));
    assert_eq!(pieces.iter().map(|p| len(&visible(p))).sum::<usize>(), 6000);
}

#[test]
fn test_a_cut_never_lands_inside_a_tag_or_an_entity() {
    let limit = 100;
    let text = format!("{}<b>bold</b> {} &amp; {}", "x".repeat(90), "y".repeat(90), "z".repeat(90)).repeat(3);
    let pieces = split(&text, limit);
    assert!(pieces.iter().all(|p| len(p) <= limit));
    let half_tag_end = Regex::new(r"<[^>]*$").unwrap();
    let half_tag_start = Regex::new(r"^[^<]*>").unwrap();
    let half_entity_end = Regex::new(r"&[a-z]*$").unwrap();
    let half_entity_start = Regex::new(r"^[a-z]*;").unwrap();
    for p in &pieces {
        assert!(balanced(p));
        assert!(!half_tag_end.is_match(p) && !half_tag_start.is_match(p)); // no half tag
        assert!(!half_entity_end.is_match(p) && !half_entity_start.is_match(p)); // no half entity
    }
    let shown: String = pieces.iter().map(|p| visible(p)).collect();
    assert_eq!(shown.replace(' ', ""), visible(&text).replace(' ', ""));
}

#[test]
fn test_visible_is_what_the_phone_shows() {
    assert_eq!(
        visible("<b>T</b>\n\na &lt; b &amp; <code>c</code> <a href=\"https://x\">link</a>"),
        "T\n\na < b & c link"
    );
}

// ---- each message kind, from a realistic sample --------------------------------------------------

#[test]
fn test_a_chat_reply_is_the_body_alone() {
    // eve's own voice in a private chat: no title, the text as it is, emoji and dashes gone
    assert_eq!(compose("Orange, like my LEDs \u{1F49B} \u{2014} warm.", None, None), "Orange, like my LEDs, warm.");
}

#[test]
fn test_a_task_result_has_a_title_and_the_goal_under_it() {
    let result = "Done. The calculator shows 1,234.\nI also noticed:\n- Mail has 3 unread\n- Slack is asking to update";
    let out = compose(result, Some("Task done"), Some("open the calculator and add up the receipts"));
    assert_eq!(
        out,
        concat!(
            "<b>Task done</b>\n",
            "<i>open the calculator and add up the receipts</i>\n",
            "\n",
            "Done. The calculator shows 1,234.\n",
            "I also noticed:\n",
            "\u{2022} Mail has 3 unread\n",
            "\u{2022} Slack is asking to update"
        )
    );
}

#[test]
fn test_a_relayed_claude_message_keeps_its_structure_with_the_repo_under_the_title() {
    let said = concat!(
        "I've fixed the failing test.\n\n",
        "**What changed**\n",
        "- `tests/test_x.py`: the timer race\n",
        "- `src/app.py`: a lock around `_flush`\n\n",
        "Run `pytest -q` to confirm; it should print `47 passed`."
    );
    assert_eq!(
        compose(said, Some("Claude"), Some("eve")),
        concat!(
            "<b>Claude</b>\n<i>eve</i>\n\n",
            "I've fixed the failing test.\n\n",
            "<b>What changed</b>\n",
            "\u{2022} <code>tests/test_x.py</code>: the timer race\n",
            "\u{2022} <code>src/app.py</code>: a lock around <code>_flush</code>\n\n",
            "Run <code>pytest -q</code> to confirm; it should print <code>47 passed</code>."
        )
    );
}

#[test]
fn test_a_permission_question_shows_the_command_as_code() {
    let out = compose("```\nrm -rf build/ && echo <done>\n```\n\nyes / no?", Some("Claude asks to run Bash"), Some("repo"));
    assert_eq!(
        out,
        concat!(
            "<b>Claude asks to run Bash</b>\n<i>repo</i>\n\n",
            "<pre>rm -rf build/ &amp;&amp; echo &lt;done&gt;</pre>\n\n",
            "yes / no?"
        )
    );
}

#[test]
fn test_a_task_question_and_a_waiting_notice_are_titled_and_short() {
    assert_eq!(
        compose("Send the email to Sam now?", Some("The task asks"), None),
        "<b>The task asks</b>\n\nSend the email to Sam now?"
    );
    assert_eq!(
        compose("Bash needs approval", Some("Claude is waiting on you"), None),
        "<b>Claude is waiting on you</b>\n\nBash needs approval"
    );
    assert_eq!(compose("", Some("Claude is waiting on you"), None), "<b>Claude is waiting on you</b>");
}

// ---- from test_watch_security.py / test_watch_reverify.py ----------------------------------------

#[test]
fn test_a_bare_url_in_an_alert_is_never_a_link() {
    // watch alerts carry page text and URLs; only a markdown [text](http..) link becomes <a href>, so a
    // page that says "https://evil.example/x" or "(https://evil.example)" shows as text
    let alert = "Price dropped to $12 at https://shop.example/item_1 (https://evil.example/?a=b&c=d)";
    assert!(!compose(alert, Some("Watching"), None).contains("<a href"));
    assert!(!compose("[x](javascript:alert(1)) and [y](ftp://z)", Some("Watch"), None).contains("<a href"));
}

// ---- beyond the Python tests ----------------------------------------------------------------------

#[test]
fn test_plain_keeps_untouched_text_byte_for_byte() {
    // nothing to strip: the text comes back as it is, doubled spaces and edges included
    assert_eq!(plain("  two  spaces  "), "  two  spaces  ");
    assert_eq!(plain("end \u{2014}"), "end");
    assert_eq!(plain("a \u{2014}, b"), "a, b");
    assert_eq!(plain("x\u{2013}y"), "x, y");
}

#[test]
fn test_python_whitespace_is_whitespace() {
    // str.isspace counts the information separators U+001C..U+001F, which Rust's \s does not
    assert_eq!(render_body("\x1c\x1dword\x1e"), "word");
    assert_eq!(compose("b", Some("\x1f"), None), "b");
}

#[test]
fn test_table_blocks_of_nothing_is_nothing() {
    let none: [&str; 0] = [];
    assert!(table_blocks(&none).is_empty());
    assert!(table_blocks(&["| a | b |", "|-|-|"]).is_empty());
}

#[test]
fn test_a_cut_that_cannot_progress_stops_instead_of_looping() {
    // Python loops forever here: the reopened tags fill the whole window at every cut
    let text = format!("<i><b><s>{}</s></b></i>", "x".repeat(40));
    let pieces = split(&text, 10);
    assert!(!pieces.is_empty());
}

// ---- differential: what Python's telegram_format returned for random inputs ----------------------

#[test]
fn test_matches_python_on_random_inputs() {
    let raw = include_str!("fixtures/differential.json");
    let cases: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap().to_string();
    let mut failures = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        let fun = case["fn"].as_str().unwrap();
        let (got, want) = match fun {
            "plain" => (serde_json::json!(plain(&s(case, "in")))
                , case["out"].clone()),
            "inline" => (serde_json::json!(inline(&s(case, "in"))), case["out"].clone()),
            "render_body" => (serde_json::json!(render_body(&s(case, "in"))), case["out"].clone()),
            "visible" => (serde_json::json!(visible(&s(case, "in"))), case["out"].clone()),
            "compose" => (
                serde_json::json!(compose(&s(case, "in"), case["title"].as_str(), case["subtitle"].as_str())),
                case["out"].clone(),
            ),
            "table_blocks" => {
                let lines: Vec<&str> = case["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
                (serde_json::json!(table_blocks(&lines)), case["out"].clone())
            }
            "split" => {
                let limit = case["limit"].as_u64().unwrap() as usize;
                (serde_json::json!(split(&s(case, "in"), limit)), case["out"].clone())
            }
            other => panic!("unknown case kind {other}"),
        };
        if got != want {
            failures.push(format!("case {n} ({fun}): input {:?}\n  rust:   {got}\n  python: {want}", case.get("in")));
        }
    }
    assert!(cases.len() > 2000, "fixture too small");
    assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), cases.len(), failures[..failures.len().min(5)].join("\n"));
}
