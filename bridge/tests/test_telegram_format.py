"""telegram_format.py: one shape for everything eve sends — escaped, titled, in paragraphs, split
without breaking a tag, and each message kind rendered from a realistic sample."""

from __future__ import annotations

import re

from eve_bridge import telegram_format as fmt
from eve_bridge.telegram_format import MAX_MESSAGE_CHARS, compose, escape, render_body, split, visible

_TAG = re.compile(r"</?([a-z]+)[^>]*>")


def _balanced(piece: str) -> bool:
    """Every tag opened in a piece is closed in it, in order."""
    stack: list[str] = []
    for m in re.finditer(r"<(/?)([a-z]+)[^>]*>", piece):
        if m.group(1):
            if not stack or stack[-1] != m.group(2):
                return False
            stack.pop()
        else:
            stack.append(m.group(2))
    return not stack


# ---- escaping ----------------------------------------------------------------------------------

def test_content_is_escaped_and_only_the_three_reserved_characters_change() -> None:
    assert escape("a < b & c > d") == "a &lt; b &amp; c &gt; d"
    assert escape('quotes "fine", café, 日本語, 3 + 4 = 7') == 'quotes "fine", café, 日本語, 3 + 4 = 7'
    # something that looks like markup in a model's answer is shown, never interpreted
    assert render_body("use <b>real</b> tags & <script>") == "use &lt;b&gt;real&lt;/b&gt; tags &amp; &lt;script&gt;"
    assert visible(render_body("a < b & c > d")) == "a < b & c > d"


def test_a_title_is_escaped_clipped_and_stripped_of_emoji_and_dashes() -> None:
    assert compose("body", "Task <done> & dusted \U0001F389") == "<b>Task &lt;done&gt; &amp; dusted</b>\n\nbody"
    assert compose("body", "Claude \u2014 eve") == "<b>Claude, eve</b>\n\nbody"
    long_title = "open the calculator and add up every receipt on the desk, then " * 3
    head = compose("body", long_title).split("\n")[0]
    assert head.startswith("<b>open the calculator") and head.endswith("\u2026</b>")
    assert len(visible(head)) <= fmt.MAX_TITLE_CHARS
    assert compose("body") == "body" and compose("body", "", None) == "body"       # no title: the body alone
    assert compose("", "Only a title") == "<b>Only a title</b>"                 # no body: the title alone


# ---- markdown to Telegram HTML -----------------------------------------------------------------

def test_markdown_from_a_model_becomes_telegram_markup_not_raw_stars_and_hashes() -> None:
    text = ("## What I did\n"
            "I fixed the **flaky test** in `tests/test_x.py` — it raced on a *timer*.\n"
            "\n"
            "- Added a lock around `_flush`\n"
            "* Removed the __sleep__ hack\n"
            "1. First\n"
            "2) Second\n"
            "\n"
            "> a quote\n"
            "---\n"
            "See [the docs](https://example.com/a?b=1&c=2) or ~~the old page~~.")
    out = render_body(text)
    assert out == ("<b>What I did</b>\n"
                   "I fixed the <b>flaky test</b> in <code>tests/test_x.py</code>, it raced on a <i>timer</i>.\n"
                   "\n"
                   "\u2022 Added a lock around <code>_flush</code>\n"
                   "\u2022 Removed the <b>sleep</b> hack\n"
                   "1. First\n"
                   "2. Second\n"
                   "\n"
                   "<i>a quote</i>\n"
                   "\n"
                   'See <a href="https://example.com/a?b=1&amp;c=2">the docs</a> or <s>the old page</s>.')
    assert "*" not in visible(out) and "#" not in visible(out) and "`" not in visible(out)


def test_code_fences_become_pre_blocks_verbatim_and_escaped() -> None:
    text = "Run this:\n```python\nif a < b and c > d:\n    print('x')   # two  spaces kept\n```\nThen `pytest -q`."
    out = render_body(text)
    assert out == ("Run this:\n\n"
                   "<pre>if a &lt; b and c &gt; d:\n    print('x')   # two  spaces kept</pre>\n\n"
                   "Then <code>pytest -q</code>.")
    # an unclosed fence is still shown as code; markdown inside code is not styled
    assert render_body("```\n**not bold** _nor italic_") == "<pre>**not bold** _nor italic_</pre>"
    assert render_body("`**kept**`") == "<code>**kept**</code>"


def test_underscores_and_stars_inside_words_are_left_alone() -> None:
    assert render_body("snake_case_name and file_name.py") == "snake_case_name and file_name.py"
    assert render_body("2*3*4 = 24") == "2*3*4 = 24"
    assert render_body("a * b") == "a * b"
    assert render_body("_italic_ and *this*") == "<i>italic</i> and <i>this</i>"


def test_a_wide_markdown_table_becomes_a_block_per_row_with_header_value_lines() -> None:
    # Codex reported tonight's showtimes as a pipe table (owner, 2026-09-24: "Format is bad"): Telegram has no
    # tables, so each row is its first cell in bold and a "header: value" line per filled cell after it
    text = ("All times below are **PM**.\n\n"
            "| Movie | Regal Thornton Place | AMC Oak Tree 6 |\n"
            "|---|---|---|\n"
            "| Avengers Endgame: Encore | 6:10 | 6:45; 8:45 (3D) |\n"
            "| Practical Magic 2 | 9:50 | |\n\n"
            "Regal Thornton Place: 316 NE Thornton Place.")
    assert render_body(text) == ("All times below are <b>PM</b>.\n\n"
                                 "<b>Avengers Endgame: Encore</b>\n"
                                 "Regal Thornton Place: 6:10\n"
                                 "AMC Oak Tree 6: 6:45; 8:45 (3D)\n\n"
                                 "<b>Practical Magic 2</b>\n"
                                 "Regal Thornton Place: 9:50\n\n"
                                 "Regal Thornton Place: 316 NE Thornton Place.")
    assert "|" not in visible(render_body(text))


def test_a_two_column_table_is_one_line_per_row_and_cells_keep_their_styles() -> None:
    text = ("| Setting | Default |\n"
            "|:--|--:|\n"
            "| `EVE_X` | 0 |\n"
            "| **Y** | *off* — for now |\n"
            "| | |\n"
            "| Z | |")
    assert render_body(text) == ("<b><code>EVE_X</code></b>: 0\n"
                                 "<b>Y</b>: <i>off</i>, for now\n"
                                 "<b>Z</b>")
    # a lone pipe line without a separator under it is prose, not a table
    assert render_body("| just a line |\nnext") == "| just a line |\nnext"
    # a table right after a fence and one ending the text both close cleanly
    assert render_body("```\nx | y\n```\n| a | b |\n|-|-|\n| 1 | 2 |") == "<pre>x | y</pre>\n\n<b>1</b>: 2"


def test_paragraphs_are_kept_blank_runs_collapsed_and_prose_is_plain() -> None:
    text = "First line  \nsecond line\n\n\n\n\nnew paragraph \u2014 with a dash \U0001F680\r\nwindows line"
    assert render_body(text) == "First line\nsecond line\n\nnew paragraph, with a dash\nwindows line"
    assert render_body("") == "" and render_body("\n\n  \n") == ""


# ---- splitting ---------------------------------------------------------------------------------

def test_short_messages_are_one_piece_and_empty_ones_none() -> None:
    assert split("hi") == ["hi"] and split("") == [] and split("  \n ") == []
    assert split("a" * MAX_MESSAGE_CHARS) == ["a" * MAX_MESSAGE_CHARS]


def test_a_long_message_is_cut_on_paragraph_boundaries_with_the_title_on_the_first_piece() -> None:
    body = "\n\n".join(f"Paragraph {i}. " + "word " * 100 for i in range(30))
    message = compose(body, "Task done", "the goal")
    pieces = split(message)
    assert len(pieces) > 1 and all(len(p) <= MAX_MESSAGE_CHARS for p in pieces)
    assert pieces[0].startswith("<b>Task done</b>\n<i>the goal</i>\n\nParagraph 0.")
    assert all(p.startswith("Paragraph ") for p in pieces[1:])
    assert all(_balanced(p) for p in pieces)
    assert "\n\n".join(pieces) == message                       # nothing lost, nothing added


def test_a_paragraph_longer_than_the_limit_is_cut_at_a_line_or_a_space_with_tags_rebalanced() -> None:
    # one <pre> block of 8000 characters: cut at line ends, closed at each cut, reopened after it
    rows = "\n".join(f"row {i:03d} " + "y" * 60 for i in range(120))
    message = compose("```\n" + rows + "\n```", "Claude")
    pieces = split(message)
    assert len(pieces) >= 3 and all(len(p) <= MAX_MESSAGE_CHARS for p in pieces)
    assert all(_balanced(p) for p in pieces)
    assert pieces[0].startswith("<b>Claude</b>\n\n<pre>row 000 ") and pieces[0].endswith("</pre>")
    assert all(p.startswith("<pre>row ") and p.endswith("</pre>") for p in pieces[1:])
    shown = "".join(visible(p).removeprefix("Claude\n\n") for p in pieces)
    assert shown.replace("\n", "") == rows.replace("\n", "")     # every character of every row arrives

    # bold that spans a cut, in one paragraph with no newline at all
    words = "<b>" + "word " * 2000 + "</b>"
    pieces = split(words)
    assert len(pieces) >= 3 and all(len(p) <= MAX_MESSAGE_CHARS for p in pieces)
    assert all(_balanced(p) and p.startswith("<b>") and p.endswith("</b>") for p in pieces)
    assert " ".join(visible(p) for p in pieces).split() == ["word"] * 2000    # pieces are separate messages

    # a link's attributes survive the reopening
    link = '<a href="https://example.com/x">' + "x" * 6000 + "</a>"
    pieces = split(link)
    assert all(p.startswith('<a href="https://example.com/x">') and p.endswith("</a>") for p in pieces)
    assert sum(len(visible(p)) for p in pieces) == 6000


def test_a_cut_never_lands_inside_a_tag_or_an_entity() -> None:
    limit = 100
    text = ("x" * 90 + "<b>bold</b> " + "y" * 90 + " &amp; " + "z" * 90) * 3
    pieces = split(text, limit)
    assert all(len(p) <= limit for p in pieces)
    for p in pieces:
        assert _balanced(p)
        assert not re.search(r"<[^>]*$", p) and not re.search(r"^[^<]*>", p)         # no half tag
        assert not re.search(r"&[a-z]*$", p) and not re.search(r"^[a-z]*;", p)      # no half entity
    assert "".join(visible(p) for p in pieces).replace(" ", "") == visible(text).replace(" ", "")


def test_visible_is_what_the_phone_shows() -> None:
    assert visible("<b>T</b>\n\na &lt; b &amp; <code>c</code> <a href=\"https://x\">link</a>") == "T\n\na < b & c link"


# ---- each message kind, from a realistic sample --------------------------------------------------

def test_a_chat_reply_is_the_body_alone() -> None:
    # eve's own voice in a private chat: no title, the text as it is, emoji and dashes gone
    assert compose("Orange, like my LEDs \U0001F49B \u2014 warm.") == "Orange, like my LEDs, warm."


def test_a_task_result_has_a_title_and_the_goal_under_it() -> None:
    result = ("Done. The calculator shows 1,234.\n"
              "I also noticed:\n"
              "- Mail has 3 unread\n"
              "- Slack is asking to update")
    out = compose(result, "Task done", "open the calculator and add up the receipts")
    assert out == ("<b>Task done</b>\n"
                   "<i>open the calculator and add up the receipts</i>\n"
                   "\n"
                   "Done. The calculator shows 1,234.\n"
                   "I also noticed:\n"
                   "\u2022 Mail has 3 unread\n"
                   "\u2022 Slack is asking to update")


def test_a_relayed_claude_message_keeps_its_structure_with_the_repo_under_the_title() -> None:
    said = ("I've fixed the failing test.\n\n"
            "**What changed**\n"
            "- `tests/test_x.py`: the timer race\n"
            "- `src/app.py`: a lock around `_flush`\n\n"
            "Run `pytest -q` to confirm; it should print `47 passed`.")
    out = compose(said, "Claude", "eve")
    assert out == ("<b>Claude</b>\n<i>eve</i>\n\n"
                   "I've fixed the failing test.\n\n"
                   "<b>What changed</b>\n"
                   "\u2022 <code>tests/test_x.py</code>: the timer race\n"
                   "\u2022 <code>src/app.py</code>: a lock around <code>_flush</code>\n\n"
                   "Run <code>pytest -q</code> to confirm; it should print <code>47 passed</code>.")


def test_a_permission_question_shows_the_command_as_code() -> None:
    out = compose("```\nrm -rf build/ && echo <done>\n```\n\nyes / no?", "Claude asks to run Bash", "repo")
    assert out == ("<b>Claude asks to run Bash</b>\n<i>repo</i>\n\n"
                   "<pre>rm -rf build/ &amp;&amp; echo &lt;done&gt;</pre>\n\n"
                   "yes / no?")


def test_a_task_question_and_a_waiting_notice_are_titled_and_short() -> None:
    assert compose("Send the email to Sam now?", "The task asks") == "<b>The task asks</b>\n\nSend the email to Sam now?"
    assert compose("Bash needs approval", "Claude is waiting on you") == "<b>Claude is waiting on you</b>\n\nBash needs approval"
    assert compose("", "Claude is waiting on you") == "<b>Claude is waiting on you</b>"
