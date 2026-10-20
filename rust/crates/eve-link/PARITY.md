# eve-link
Status: partial

Eve Link's daemon half: five Python modules (3889 lines). The architect committed the skeleton (every type, constant,
prompt and signature; prompts checked byte for byte against the Python). Four fill units port the bodies (see "Work
split"). Each unit records its tests, boundaries and differences in its own fragment under `parity/`
(`parity/<unit>.md`). Integration merges the fragments into the sections below and sets `Status: complete` after
`cargo test -p eve-link`, clippy and `python3 tools/check_port.py --crate link` pass.

## Work split
| Unit | Python | Rust files | Tests file | Python tests |
|---|---|---|---|---|
| chat-core | link_chat.py 1-735 (paths, limits, the OpenUI guide, the transcript read back, sse, the chief's cards, HiddenJobs), link_topics.py | src/link_chat/mod.rs, src/link_topics.rs | tests/chat_core.rs | test_link_topics.py; the test_link_chat.py tests that call module functions directly (strip_openui, history_text, app_block, read_messages, read_before, split_cursor, controls_listing, list_jobs, job_*, HiddenJobs, JOB_LEAD) |
| chat-door | link_chat.py 737-1677 (LinkChat), link_today.py | src/link_chat/door.rs, src/link_today.rs | tests/chat_door.rs | test_link_today.py; the test_link_chat.py tests that go through the HTTP door |
| updates | updates.py | src/updates.rs | tests/updates.rs | test_updates.py |
| updates-composio | updates_composio.py | src/updates_composio.rs | tests/updates_composio.rs | test_updates_composio.py |

Architect-complete (no unit edits them): `Cargo.toml`, `src/lib.rs` (Env, env_get, expanduser, monotonic, Mono, Now,
local_now), `src/seams.rs` (Inlet, Chief/JobCard, MiniApp/Hub/AppStoreView, DirectApps, ComposioRead, WorkDoor, and
their impls for AppsBridge, ComposioBridge and WorkSessions), and in `src/link_chat/mod.rs` `reason`, `py_json`,
`prompt_path` and every constant and regex.

What Python reached by duck typing on another module's object is a trait in `seams.rs`: the brain implements
`Inlet` (and `Chief` for the chief, `MiniApp`/`Hub` for the Mini App server and its cards). Python's tests
that need the real TelegramInlet, Chief or MiniAppServer run here against fakes of those traits; what only the real
objects can show is listed as not ported, with the crate that will test it.

## Modules
| Python | Rust |
|---|---|
| link_chat.py | src/link_chat/mod.rs, src/link_chat/door.rs |
| link_today.py | src/link_today.rs |
| link_topics.py | src/link_topics.rs |
| updates.py | src/updates.rs |
| updates_composio.py | src/updates_composio.rs |

## API
| Python | Rust | Notes |
|---|---|---|
| link_chat.controls_listing | link_chat::controls_listing |  |
| link_chat.app_block | link_chat::app_block |  |
| link_chat.strip_openui | link_chat::strip_openui |  |
| link_chat.history_text | link_chat::history_text |  |
| link_chat._row | link_chat::row |  |
| link_chat.att_record | link_chat::att_record |  |
| link_chat._att | link_chat::att |  |
| link_chat.read_messages | link_chat::read_messages |  |
| link_chat.read_before | link_chat::read_before |  |
| link_chat.split_cursor | link_chat::split_cursor |  |
| link_chat._length | link_chat::length |  |
| link_chat.turn_id | link_chat::turn_id |  |
| link_chat._Turn | link_chat::door::Turn (crate-private) |  |
| link_chat.sse | link_chat::sse |  |
| link_chat._chunk | link_chat::chunk |  |
| link_chat._go_ready | link_chat::go_ready |  |
| link_chat.job_needs_you | link_chat::job_needs_you |  |
| link_chat.job_actions | link_chat::job_actions |  |
| link_chat.job_line | link_chat::job_line |  |
| link_chat.job_entry | link_chat::job_entry |  |
| link_chat.list_jobs | link_chat::list_jobs |  |
| link_chat.HiddenJobs | link_chat::HiddenJobs |  |
| link_chat.LinkChat | link_chat::LinkChat (door.rs) | private methods are door.rs private fns |
| link_today._clip | link_today::clip |  |
| link_today.jobs_part | link_today::jobs_part |  |
| link_today.watches_part | link_today::watches_part |  |
| link_today._moment | link_today::moment |  |
| link_today.events_part | link_today::events_part |  |
| link_today.music_part | link_today::music_part |  |
| link_today.tv_part | link_today::tv_part |  |
| link_today.Today | link_today::Today |  |
| link_topics.plain | link_topics::plain |  |
| link_topics.clip | link_topics::clip |  |
| link_topics.small_talk | link_topics::small_talk |  |
| link_topics.segment | link_topics::segment |  |
| link_topics.read_rows | link_topics::read_rows |  |
| link_topics.Bookmarks | link_topics::Bookmarks |  |
| link_topics.Titler | link_topics::Titler |  |
| link_topics.view | link_topics::view |  |
| link_topics.known | link_topics::known |  |
| updates.enabled | updates::enabled |  |
| updates.every_secs | updates::every_secs |  |
| updates._one_line | updates::one_line, one_line_str |  |
| updates._clip | updates::clip |  |
| updates._clip_body | updates::clip_body |  |
| updates.local_zone | updates::local_zone |  |
| updates.item | updates::item |  |
| updates._structured | updates::structured |  |
| updates.mail_items | updates::mail_items |  |
| updates._owners | updates::owners |  |
| updates._thread_text | updates::thread_text |  |
| updates._mail_date | updates::mail_date |  |
| updates.calendar_items | updates::calendar_items |  |
| updates.event_item | updates::event_item |  |
| updates._open_todos | updates::open_todos |  |
| updates.note_items | updates::note_items |  |
| updates.journal_open | updates::journal_open |  |
| updates.memory_items | updates::memory_items |  |
| updates.chief_items | updates::chief_items |  |
| updates.drop_hidden | updates::drop_hidden |  |
| updates.Sources | updates::Sources, UpdateSources |  |
| updates._records_dir | updates::default_records_dir |  |
| updates._hidden_match | updates::default_hidden_match |  |
| updates.gather | updates::gather |  |
| updates.here | updates::here |  |
| updates.SuggestError | updates::SuggestError |  |
| updates.prompt_input | updates::prompt_input |  |
| updates.card_id | updates::card_id |  |
| updates._verb | updates::verb_of |  |
| updates._text | updates::text_of |  |
| updates._due | updates::due_of |  |
| updates.carries_payload | updates::carries_payload |  |
| updates.ref_kind | updates::ref_kind |  |
| updates.clean_cards | updates::clean_cards |  |
| updates.suggest | updates::suggest |  |
| updates.reply | updates::reply |  |
| updates.Client | updates::Client, SuggestClient |  |
| updates.make_client | updates::make_client |  |
| updates.UpdatesStore | updates::UpdatesStore, StoreState |  |
| updates.live_dismissed | updates::live_dismissed |  |
| updates.served | updates::served |  |
| updates.lost_kinds | updates::lost_kinds |  |
| updates.merge_cards | updates::merge_cards |  |
| updates.card_hidden | updates::card_hidden |  |
| updates._parse_at | updates::parse_at |  |
| updates.Updates | updates::Updates, UpdatesOptions |  |
| updates_composio.SourceOff | updates_composio::source_off (updates::ReadError::Off) |  |
| updates_composio.ReadFailed | updates_composio::read_failed (updates::ReadError::Failed) |  |
| updates_composio.enabled | updates_composio::enabled |  |
| updates_composio.usable | updates_composio::usable |  |
| updates_composio.safe_link | updates_composio::safe_link |  |
| updates_composio._linked | updates_composio::linked |  |
| updates_composio.alias_word | updates_composio::alias_word |  |
| updates_composio.Reader | updates_composio::Reader |  |
| updates_composio._iso | updates_composio::iso |  |
| updates_composio._parse | updates_composio::parse |  |
| updates_composio._day | updates_composio::day |  |
| updates_composio.slack_text | updates_composio::slack_text |  |
| updates_composio._matches | updates_composio::matches |  |
| updates_composio._slack_ok | updates_composio::slack_ok |  |
| updates_composio.slack_items | updates_composio::slack_items |  |
| updates_composio._plain | updates_composio::plain |  |
| updates_composio._mentions | updates_composio::mentions |  |
| updates_composio.notion_page | updates_composio::notion_page |  |
| updates_composio.notion_blocks | updates_composio::notion_blocks |  |
| updates_composio.notion_items | updates_composio::notion_items |  |
| updates_composio.linear_items | updates_composio::linear_items |  |
| updates_composio._labels | updates_composio::labels |  |
| updates_composio.work_email_items | updates_composio::work_email_items |  |
| updates_composio.work_calendar_items | updates_composio::work_calendar_items |  |
| updates_composio.reads | updates_composio::reads |  |

## Tests
(merged from parity/ at integration)

## Boundaries
(merged from parity/ at integration)

## Differences
(merged from parity/ at integration)
