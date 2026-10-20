# eve-link: unit updates-composio
Status: complete

`updates_composio.py` (562 lines) → `src/updates_composio.rs`. Python tests: `bridge/tests/test_updates_composio.py`
→ `tests/updates_composio.rs`.

## API notes
These rows add to the crate's API table. The table's other `updates_composio` rows hold as written.

| Python | Rust | Notes |
|---|---|---|
| updates_composio.READERS | updates_composio::READERS (the kinds) + updates_composio::read_kind | Python's dict of functions becomes its key set plus one dispatcher. `read_kind("notion", …)` reads with no deadline, as `READERS["notion"](reader, now)` did. |
| updates_composio.Reader.accounts / accounts_for / call | Reader::accounts / accounts_for / call (async) | The listing is held under a `tokio::sync::Mutex` across the call, so a second source waits for the first one's answer, as Python's `threading.Lock` made it. A failed listing is cached by its type name (`ComposioError::type_name`). |
| (monkeypatch of READ_SLUGS) | Reader::with_allowed | The test seam for the allow-list. `Reader::new` uses READ_SLUGS. |
| updates_composio._iso | updates_composio::iso, updates_composio::try_iso | `try_iso` raises Python's errors for a time that no datetime can hold. The sources call `try_iso`. |
| updates_composio.notion_page / notion_blocks | notion_page / notion_blocks, plus try_notion_page / try_notion_blocks | The `try_` forms raise Python's TypeError when a `people` or `results` value is a number or true. `notion_items` calls them. |
| urllib.parse.urlsplit (inside safe_link) | private `urlsplit` | Ported as Python 3.13 parses printable ASCII: scheme detection, the netloc up to `/?#`, the bracket and IPvFuture checks, and `_hostinfo`'s hostname and port. |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| test_updates_composio.py | test_slack_keeps_unanswered_dms_and_mentions_one_per_conversation, test_notion_reads_open_todos_mentions_and_assigned_open_tasks, test_notion_reads_no_blocks_after_its_deadline, test_linear_keeps_open_issues_due_soon_or_recently_updated, test_work_email_reads_the_work_account_only_one_item_per_thread, test_work_calendar_reads_like_the_owners_calendar, test_no_other_account_means_the_work_kinds_are_off, test_links_are_kept_only_on_the_apps_own_https_hosts, test_every_composio_source_lands_in_gather_with_status_ok, test_the_model_never_reads_an_items_link, test_off_without_a_bridge_or_with_the_switch_off, test_a_bridge_that_cannot_read_is_off, test_a_toolkit_that_is_not_connected_is_off_and_the_rest_still_read, test_an_expired_connection_fails_and_is_lost, test_a_failing_call_fails_its_source_only, test_an_unreadable_listing_fails_every_composio_source, test_a_source_that_goes_off_after_a_full_build_is_lost, test_the_accounts_are_listed_once_per_gather, test_hidden_words_and_dismissed_refs_drop_composio_items, test_every_allowed_slug_only_reads, test_a_sending_slug_is_refused_before_it_reaches_composio, test_the_consequential_check_guards_even_an_allow_listed_slug, test_every_slug_the_sources_run_is_on_the_allow_list, test_clean_cards_accepts_every_composio_ref_and_keeps_the_link, test_a_card_citing_an_unknown_composio_ref_is_dropped, test_a_merged_card_keeps_the_link_of_its_first_ref_that_has_one, test_a_served_card_never_carries_a_link_off_its_apps_host, test_a_rebuild_keeps_a_lost_composio_sources_old_cards, test_the_prompt_names_the_kinds_as_one_positive_set, test_updates_knows_every_composio_kind_and_ref_prefix | the same names in tests/updates_composio.rs, plus test_safe_link_reads_the_url_as_urlsplit_does, test_small_readers, test_notion_page_reads_its_properties, test_reads_share_one_listing_and_are_off_without_a_usable_bridge, test_an_expired_only_connection_fails_and_a_missing_one_is_off | test_the_bridge_refuses_a_writing_slug_itself and test_connected_accounts_leaves_direct_apps_out test `composio_tools.ComposioBridge` itself. They are ported under the same names in eve-composio-tools (src/tests.rs), because they need the bridge's private client field. |

The fake bridge answers from the shared fixture `bridge/tests/fixtures/updates/composio/responses.json`, which is
read with `include_str!`. The test sources stand in for Python's `Sources` subclass: no direct apps, no vault and no
chief. They read the bridge, the hidden words and the records folder through the real `updates::Sources`, so the
`EVE_UPDATES_COMPOSIO` switch and `usable` are the production checks.

In test_a_rebuild_keeps_a_lost_composio_sources_old_cards, Python's `u._next_try > 0` (a private field) becomes
`!u.due(later, true)`. The cache is 3 hours old, which is past the 2-hour default interval, so only the retry wait
that the lost build set can keep the rebuild from being due.

Parity was checked beyond the assertions. The five sources ran on the fixture in Python 3.13 (the bridge venv) and in
Rust. The items and the exact calls each one made (slug, arguments with their key order, account) were the same JSON,
key order included.

The gather, clean_cards, served and rebuild tests also run updates.rs, which is the updates unit's work. They passed
against that unit's code as it stood when this unit was finished.

## Boundaries
None.

## Differences
- `safe_link` with a malformed port (`:ab`, `:99999`): Python's `parts.port` raised ValueError outside the
  `try`, so one bad permalink failed the whole source. Here that URL gives no link, and the item is kept without one.
  The signature `Option<String>` cannot raise. An app's own permalinks never carry a port.
- `iso` (the public helper) returns "" for a time that no datetime can hold, where Python raised. Every source calls
  `try_iso`, which fails the source as Python did: ValueError for a year past 9999, OverflowError past time_t or
  when the local shift leaves the datetime range, OSError past what gmtime holds. The kinds are only logged.
- `notion_page` / `notion_blocks` (the public helpers) read a non-iterable `people` / `results` as empty, where
  Python raised TypeError. `notion_items` calls the `try_` forms and fails like Python.
- The label of a work account (`alias.strip().capitalize()`) title-cases the first character with a table of
  the Latin and Armenian characters whose title case differs from their upper case (ǅ, ǈ, ǋ, ǲ, ß → "Ss", the ﬀ–ﬆ
  ligatures, ŉ, և). Rare Greek characters with a ypogegrammeni use their upper case. An alias is a plain word such as
  "work", so this never shows.
