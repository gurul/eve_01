//! Port of bridge/tests/test_spend.py: the ledger, aggregation, prices, helpers and the chief's card tag. The
//! spend_sync tests in that file belong to eve-spend-sync.

use super::*;
use chrono::{Local, NaiveDate, TimeZone};
use serde_json::json;
use std::sync::{Arc, Mutex as StdMutex};

const M: i64 = 1_000_000;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn noon(d: NaiveDate) -> f64 {
    Local.from_local_datetime(&d.and_hms_opt(12, 0, 0).unwrap()).earliest().unwrap().timestamp() as f64
}

/// The `_spend_ledger_in_tmp` fixture: a ledger in its own temp folder.
fn ledger() -> (tempfile::TempDir, Ledger) {
    let tmp = tempfile::tempdir().unwrap();
    let l = Ledger::at(tmp.path());
    (tmp, l)
}

fn rows(l: &Ledger, d: Option<NaiveDate>) -> Vec<Map<String, Value>> {
    l.day_rows(&iso(d.unwrap_or_else(today_local)))
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Runs `f` with a tracing subscriber that captures the log text (caplog).
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone)]
    struct Buf(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(StdMutex::new(Vec::new())));
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt().with_writer(move || writer.clone()).with_ansi(false).finish();
    tracing::subscriber::with_default(sub, f);
    let out = buf.0.lock().unwrap().clone();
    String::from_utf8(out).unwrap()
}

// ---- the ledger --------------------------------------------------------------------------------------------------

#[test]
fn test_a_record_is_one_tiny_line_in_the_local_days_file() {
    let (tmp, l) = ledger();
    l.record(
        "openai",
        "gpt-6-luna",
        CHAT,
        Some(0.00042),
        RecordOpts::new()
            .tokens([("in", json!(3100)), ("cached", json!(2900)), ("out", json!(80)), ("zero", json!(0))])
            .now(noon(day())),
    );
    let text = std::fs::read_to_string(tmp.path().join("2026-09-24.jsonl")).unwrap();
    let line: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        line,
        json!({"t": noon(day()) as i64, "p": "openai", "m": "gpt-6-luna", "f": "chat", "usd": 0.00042,
               "src": "priced", "tok": {"in": 3100, "cached": 2900, "out": 80}})
    );
    // Byte for byte what Python's json.dumps(row, separators=(",", ":")) writes.
    assert_eq!(
        text,
        format!(
            "{{\"t\":{},\"p\":\"openai\",\"m\":\"gpt-6-luna\",\"f\":\"chat\",\"usd\":0.00042,\"src\":\"priced\",\
             \"tok\":{{\"in\":3100,\"cached\":2900,\"out\":80}}}}\n",
            noon(day()) as i64
        )
    );
}

#[test]
fn test_an_unknown_price_is_recorded_as_none_and_flagged_never_guessed() {
    let (_tmp, l) = ledger();
    let logs = capture_logs(|| {
        for _ in 0..2 {
            l.record("openai", "gpt-9-unknown", CHAT, None, RecordOpts::new().now(noon(day())));
        }
    });
    let got = rows(&l, Some(day()));
    assert_eq!(got.iter().map(|r| r["usd"].clone()).collect::<Vec<_>>(), vec![Value::Null, Value::Null]);
    assert!(got.iter().all(|r| r["unpriced"] == json!(true)));
    assert_eq!(logs.matches("gpt-9-unknown").count(), 1); // logged once per model
    assert!(logs.contains("spend: no price for openai model gpt-9-unknown (chat); recorded unpriced"));
}

#[test]
fn test_record_never_raises_when_the_folder_cannot_be_written() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    let l = Ledger::at(blocker.join("spend")); // a folder under a file: mkdir fails
    l.record("openai", "gpt-6-luna", CHAT, Some(0.1), RecordOpts::new()); // no panic
    assert!(!blocker.join("spend").exists());
}

#[test]
fn test_concurrent_records_are_all_kept_whole() {
    let (_tmp, l) = ledger();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let l = l.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    l.record("openai", "gpt-5.4-nano", CAMERA, Some(0.0001), RecordOpts::new().tokens([("in", json!(100))]));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let got = rows(&l, None);
    assert_eq!(got.len(), 1600);
    assert!(approx(totals(&got).usd, 0.16));
}

#[test]
fn test_a_torn_line_is_skipped() {
    let (tmp, l) = ledger();
    l.record("openai", "gpt-6-luna", CHAT, Some(0.5), RecordOpts::new().now(noon(day())));
    let mut f = OpenOptions::new().append(true).open(tmp.path().join("2026-09-24.jsonl")).unwrap();
    f.write_all(br#"{"t": 1, "p": "open"#).unwrap();
    assert_eq!(rows(&l, Some(day())).len(), 1);
}

#[test]
fn test_the_ledger_line_carries_no_words() {
    let (tmp, l) = ledger();
    let secret = "please remember my bank pin";
    l.record_response(
        CHAT,
        &json!({"model": "gpt-6-luna", "usage": {"input_tokens": 10, "output_tokens": 5},
                "output": [{"type": "message", "content": [{"type": "output_text", "text": secret}]}]}),
        "",
        "openrouter",
        None,
    );
    let text = std::fs::read_to_string(tmp.path().join(format!("{}.jsonl", iso(today_local())))).unwrap();
    assert!(!text.contains(secret));
}

// ---- aggregation -------------------------------------------------------------------------------------------------

#[test]
fn test_totals_split_by_provider_feature_and_model_largest_first() {
    let lines = vec![
        obj(json!({"p": "openai", "m": "gpt-6-luna", "f": "chat", "usd": 0.2})),
        obj(json!({"p": "openai", "m": "gpt-live-1", "f": "voice", "usd": 0.5})),
        obj(json!({"p": "openrouter", "m": "google/gemini-3.1-flash-lite", "f": "search", "usd": 0.01})),
        obj(json!({"p": "chatgpt", "m": "codex", "f": "codex", "usd": null, "unpriced": true})),
    ];
    let t = totals(&lines);
    assert!(approx(t.usd, 0.71) && t.calls == 4 && t.unpriced == 1);
    assert_eq!(t.by_provider.keys().collect::<Vec<_>>(), ["openai", "openrouter", "chatgpt"]);
    assert_eq!(t.by_feature.keys().collect::<Vec<_>>(), ["voice", "chat", "search", "codex"]);
    assert!(approx(t.by_model["gpt-live-1"], 0.5));
    assert_eq!(t.unpriced_models, vec!["chatgpt/codex".to_string()]);
}

#[test]
fn test_summary_has_today_yesterday_thirty_days_and_the_month() {
    let (tmp, l) = ledger();
    let d = day();
    l.record("openai", "gpt-6-luna", CHAT, Some(1.0), RecordOpts::new().now(noon(d)));
    l.record("openai", "gpt-live-1", VOICE, Some(0.5), RecordOpts::new().now(noon(d)));
    l.record("openai", "gpt-6-luna", CHAT, Some(2.0), RecordOpts::new().now(noon(d - Duration::days(1))));
    l.record("openai", "gpt-6-luna", CHAT, Some(4.0), RecordOpts::new().now(noon(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap())));
    l.record("openai", "gpt-6-luna", CHAT, Some(8.0), RecordOpts::new().now(noon(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()))); // last month
    let s = summary(Some(d), 30, Some(tmp.path()));
    assert!(approx(s.today.totals.usd, 1.5));
    assert_eq!(s.today.totals.by_feature, IndexMap::from([("chat".to_string(), 1.0), ("voice".to_string(), 0.5)]));
    assert!(approx(s.yesterday.totals.usd, 2.0) && s.yesterday.date == "2026-09-23");
    assert_eq!(s.month.label, "2026-09");
    assert!(approx(s.month.usd, 7.5));
    assert_eq!(s.days.len(), 30);
    assert_eq!(s.days[29], DayUsd { date: "2026-09-24".into(), usd: 1.5 });
    assert_eq!(s.days[0].date, "2026-08-26");
    assert_eq!(s.days[5], DayUsd { date: "2026-08-31".into(), usd: 8.0 });
    assert_eq!(s.days.iter().filter(|x| x.usd == 0.0).count(), 26); // empty days are zeros, not gaps
    // The JSON has Python's keys, date first in each day.
    let v = serde_json::to_value(&s).unwrap();
    let keys: Vec<&String> = v["today"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["date", "usd", "calls", "unpriced", "unpriced_models", "by_provider", "by_feature", "by_model"]);
    assert_eq!(v["month"], json!({"label": "2026-09", "usd": 7.5}));
}

#[test]
fn test_brief_is_written_by_code_with_the_top_three_features_and_openrouters_figure() {
    let lines: Vec<Map<String, Value>> = [("chat", 0.3), ("voice", 1.2), ("search", 0.05), ("camera", 0.01)]
        .iter()
        .map(|(f, u)| obj(json!({"p": "openai", "f": f, "m": "m", "usd": u})))
        .collect();
    let mut today = serde_json::to_value(totals(&lines)).unwrap();
    today["date"] = json!("2026-09-24");
    let s = json!({"today": today, "yesterday": {"usd": 2.5}, "month": {"usd": 40.0}});
    let providers =
        json!({"openrouter": {"status": "ok", "key_today": 0.12, "account_today": 0.4, "account_partial": false}});
    let text = brief(&s, Some(&providers));
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "Spending (eve's own meter)",
            "Today: $1.56",
            "Yesterday: $2.50",
            "This month: $40.00",
            "Top today: voice $1.20, chat $0.30, search $0.050",
            "OpenRouter says today (UTC): $0.12 on eve's key, $0.40 on the whole account.",
        ]
    );
}

#[test]
fn brief_counts_unpriced_calls_partial_accounts_and_an_openrouter_error() {
    let s = json!({"today": {"usd": 0, "by_feature": {}, "unpriced": 1}, "yesterday": {"usd": null},
                   "month": {"usd": 0.05}});
    let ok = json!({"openrouter": {"status": "ok", "key_today": 0.0, "account_today": 0.004, "account_partial": true}});
    let lines: Vec<String> = brief(&s, Some(&ok)).lines().map(String::from).collect();
    assert_eq!(lines[1..4], ["Today: $0.00", "Yesterday: ?", "This month: $0.050"]);
    assert_eq!(
        lines[4],
        "1 call today had no price (Codex on the ChatGPT plan, or a model without a rate); not in the totals."
    );
    assert_eq!(lines[5], "OpenRouter says today (UTC): $0.00 on eve's key, $0.004 on the whole account so far.");
    let err = json!({"openrouter": {"status": "error", "error": "HTTP 403"}});
    assert!(brief(&s, Some(&err)).ends_with("OpenRouter's own figure could not be read (HTTP 403)."));
    assert_eq!(brief(&s, None).lines().count(), 5);
    // Python's `n == 1` holds for a float 1.0 too: "1.0 call", not "1.0 calls".
    let f = json!({"today": {"usd": 0, "by_feature": {}, "unpriced": 1.0}, "yesterday": {"usd": 0}, "month": {"usd": 0}});
    assert!(brief(&f, None).contains("\n1.0 call today had no price"));
    let two = json!({"today": {"usd": 0, "by_feature": {}, "unpriced": 2}, "yesterday": {"usd": 0}, "month": {"usd": 0}});
    assert!(brief(&two, None).contains("\n2 calls today had no price"));
}

// ---- the prices --------------------------------------------------------------------------------------------------

#[test]
fn test_every_responses_model_eve_uses_has_its_grounded_rate() {
    let usage = json!({"input_tokens": 2 * M, "input_tokens_details": {"cached_tokens": M}, "output_tokens": M});
    for (model, inp, cached, out) in [
        ("gpt-6-astra", 10.0, 1.0, 50.0),
        ("gpt-6-sol", 2.0, 0.20, 10.0),
        ("gpt-6-luna", 0.10, 0.01, 0.50),
        ("gpt-5-mini", 0.25, 0.025, 2.00),
        ("gpt-5.4-nano", 0.20, 0.02, 1.25),
    ] {
        assert!(approx(eve_pricing::estimate_openai_cost(model, &usage).unwrap(), inp + cached + out), "{model}");
    }
}

#[test]
fn test_live_minutes_transcription_embeddings_and_opus() {
    use eve_pricing::*;
    assert!(approx(estimate_live_cost("gpt-live-1", 90.0).unwrap(), 0.075));
    assert!(approx(
        estimate_transcribe_cost("gpt-4o-mini-transcribe", Some(&json!({"input_tokens": M, "output_tokens": M})), 0.0)
            .unwrap(),
        1.25 + 5.00
    ));
    assert!(approx(estimate_transcribe_cost("gpt-4o-mini-transcribe", Some(&json!({})), 120.0).unwrap(), 0.006));
    assert!(approx(
        estimate_transcribe_cost("gpt-4o-mini-transcribe", Some(&json!({"type": "duration", "seconds": 60})), 0.0)
            .unwrap(),
        0.003
    ));
    assert!(approx(estimate_embedding_cost("text-embedding-3-small", M).unwrap(), 0.02));
    let opus = json!({"input_tokens": M, "output_tokens": M, "cache_read_input_tokens": M,
                      "cache_creation_input_tokens": 2 * M,
                      "cache_creation": {"ephemeral_5m_input_tokens": M, "ephemeral_1h_input_tokens": M}});
    assert!(approx(estimate_anthropic_cost("claude-opus-5-5", &opus).unwrap(), 4.0 + 20.0 + 0.20 + 5.0 + 8.0));
}

#[test]
fn test_a_model_without_a_rate_prices_to_none_but_a_pinned_suffix_is_the_same_model() {
    use eve_pricing::*;
    assert_eq!(estimate_live_cost("gpt-live-2", 60.0), None);
    assert_eq!(estimate_embedding_cost("text-embedding-3-large", 10), None);
    assert_eq!(estimate_anthropic_cost("claude-sonnet-5", &json!({"input_tokens": 1})), None);
    assert_eq!(estimate_transcribe_cost("whisper-1", Some(&json!({"input_tokens": 1})), 0.0), None);
    assert!(approx(estimate_anthropic_cost("claude-opus-5-5-20260801", &json!({"output_tokens": M})).unwrap(), 20.0));
    assert!(approx(estimate_openai_cost("openai/gpt-6-astra-2026-08-01", &json!({"output_tokens": M})).unwrap(), 50.0));
}

// ---- the helpers -------------------------------------------------------------------------------------------------

#[test]
fn test_a_responses_reply_is_priced_with_its_hosted_searches() {
    let (_tmp, l) = ledger();
    l.record_response(
        THINKING,
        &json!({"model": "gpt-6-sol-2026-09-01", "usage": {"input_tokens": M, "output_tokens": 0},
                "output": [{"type": "web_search_call"}, {"type": "web_search_call"}]}),
        "",
        "openrouter",
        None,
    );
    let got = rows(&l, None);
    assert_eq!(got.len(), 1);
    let row = &got[0];
    assert!(approx(row["usd"].as_f64().unwrap(), 2.0 + 0.02));
    assert_eq!(row["tok"], json!({"in": M, "searches": 2}));
    assert_eq!(row["src"], "priced");
    assert_eq!(row["f"], "thinking");
}

#[test]
fn test_a_providers_own_cost_wins_and_a_reply_without_usage_is_unpriced() {
    let (_tmp, l) = ledger();
    l.record_chat_completion(
        SEARCH,
        &json!({"usage": {"prompt_tokens": 900, "completion_tokens": 60, "cost": 0.0061}}),
        "google/gemini-3.1-flash-lite",
        "openrouter",
    );
    l.record_chat_completion(
        LESSONS,
        &json!({"usage": {"prompt_tokens": M, "completion_tokens": 0}}),
        "openai/gpt-6-astra",
        "openrouter",
    );
    l.record_response(CHAT, &json!({"model": "gpt-6-luna"}), "", "openrouter", None);
    let got = rows(&l, None);
    let (a, b, c) = (&got[0], &got[1], &got[2]);
    assert_eq!((a["usd"].clone(), a["src"].clone(), a["p"].clone()), (json!(0.0061), json!("reported"), json!("openrouter")));
    assert!(approx(b["usd"].as_f64().unwrap(), 10.0) && b["src"] == "priced");
    assert!(c["usd"].is_null() && c["unpriced"] == json!(true) && c["note"] == "no usage in the reply");
}

#[test]
fn test_an_sdk_object_is_read_like_a_dict() {
    #[derive(Serialize)]
    struct Usage {
        input_tokens: i64,
        output_tokens: i64,
        cost: Option<f64>,
    }
    #[derive(Serialize)]
    struct Reply {
        model: String,
        usage: Usage,
    }
    let (_tmp, l) = ledger();
    let reply = Reply { model: "gpt-5.4-nano".into(), usage: Usage { input_tokens: M, output_tokens: 0, cost: None } };
    // exclude_none: the null cost is dropped, so it is priced, not "reported".
    l.record_response(CAMERA, &sdk_dump(&reply), "", "openrouter", None);
    let got = rows(&l, None);
    assert!(approx(got[0]["usd"].as_f64().unwrap(), 0.20));
    assert_eq!(got[0]["src"], "priced");
}

#[test]
fn anthropic_embedding_transcription_and_live_helpers_write_their_rows() {
    let (_tmp, l) = ledger();
    let usd = l.record_anthropic(
        MEMORY,
        "claude-opus-5-5",
        &json!({"input_tokens": M, "output_tokens": 0, "cache_read_input_tokens": 0}),
        "openrouter",
    );
    assert!(approx(usd.unwrap(), 4.0));
    assert_eq!(l.record_anthropic(MEMORY, "claude-opus-5-5", &json!({"input_tokens": 5, "cost": 0.5}), "openrouter"), Some(0.5));
    l.record_embedding(MEMORY, "text-embedding-3-small", &json!({"usage": {"prompt_tokens": 0, "total_tokens": M}}), "openrouter");
    l.record_embedding(MEMORY, "text-embedding-3-small", &json!({"data": []}), "openrouter");
    l.record_transcription(TRANSCRIPTION, "gpt-4o-mini-transcribe", &json!({}), 120.0, "openrouter");
    l.record_live(VOICE, "gpt-live-1", 2.5, "desk");
    let got = rows(&l, None);
    assert_eq!(got.len(), 6);
    assert_eq!(got[0]["tok"], json!({"in": M}));
    assert_eq!((got[1]["src"].clone(), got[1]["usd"].clone()), (json!("reported"), json!(0.5)));
    assert!(approx(got[2]["usd"].as_f64().unwrap(), 0.02) && got[2]["tok"] == json!({"in": M}));
    assert!(got[3]["usd"].is_null()); // no usage: unpriced, never guessed
    assert!(approx(got[4]["usd"].as_f64().unwrap(), 0.006));
    assert_eq!(got[5]["p"], "openai");
    assert_eq!(got[5]["tok"], json!({"secs": 2})); // round half to even, as Python's round
    assert_eq!(got[5]["note"], "desk");
}

#[test]
fn record_clips_fields_normalizes_notes_and_writes_python_floats() {
    let (tmp, l) = ledger();
    l.record("", &"x".repeat(100), "", Some(0.000012345678912), RecordOpts::new().note("  a\n\tb  é ").now(noon(day())));
    l.record("p", "m", "f", Some(-1.0), RecordOpts::new().note("refund").now(noon(day())));
    let text = std::fs::read_to_string(tmp.path().join("2026-09-24.jsonl")).unwrap();
    let first = text.lines().next().unwrap();
    assert!(first.contains(&format!("\"p\":\"?\",\"m\":\"{}\",\"f\":\"other\",\"usd\":1.235e-05", "x".repeat(64))));
    assert!(first.ends_with(",\"note\":\"a b \\u00e9\"}"));
    let got = rows(&l, Some(day()));
    assert!(got[1]["usd"].is_null() && got[1]["note"] == "refund");
}

#[test]
fn python_float_repr_matches() {
    for (x, want) in [
        (0.00042, "0.00042"),
        (1.0, "1.0"),
        (1e-5, "1e-05"),
        (1e16, "1e+16"),
        (1.5e16, "1.5e+16"),
        (1e15, "1000000000000000.0"),
        (123.456, "123.456"),
        (0.0001, "0.0001"),
        (-0.0, "-0.0"),
        (f64::INFINITY, "Infinity"),
    ] {
        assert_eq!(py_float_repr(x), want);
    }
    assert_eq!(py_round(0.123456789, 8), 0.12345679);
    assert_eq!(money(Some(0.125)), "$0.12"); // half to even, as Python's format
}

#[test]
fn spend_dir_reads_the_env_value_and_expands_home() {
    assert_eq!(spend_dir_from(Some("/x/spend")), PathBuf::from("/x/spend"));
    let default = spend_dir_from(Some("  "));
    assert!(default.ends_with(".config/eve-bridge/spend") && !default.starts_with("~"));
    assert_eq!(default, spend_dir_from(None));
}

#[test]
fn day_rows_reads_universal_newlines_and_skips_foreign_lines() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("2026-09-24.jsonl"), b"{\"usd\":1}\r\n[1,2]\n\xff\xfe\n{\"usd\":2}\r{\"usd\":3}").unwrap();
    let got = day_rows("2026-09-24", Some(tmp.path()));
    assert_eq!(got.iter().map(|r| r["usd"].as_i64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(day_rows("2026-09-25", Some(tmp.path())).is_empty());
}

// ---- the chief's card tag (spend.job; chief design G3.3, 2026-09-29) ----------------------------------------------

#[test]
fn test_rows_written_inside_a_job_carry_its_card_id_and_rows_outside_do_not() {
    let (tmp, l) = ledger();
    let at = || RecordOpts::new().now(noon(day()));
    l.record("openai", "gpt-6-luna", CHAT, Some(0.001), at());
    {
        let _c7 = job("c7");
        assert_eq!(current_job(), "c7");
        l.record("openrouter", "m", CHIEF, Some(0.002), at());
        {
            let _c8 = job("c8");
            l.record("openrouter", "m", CHIEF, Some(0.003), at());
        }
        l.record("openrouter", "m", CHIEF, Some(0.004), at());
    }
    l.record("openai", "gpt-6-luna", CHAT, Some(0.005), at());
    assert_eq!(current_job(), "");
    let got: Vec<(Option<String>, f64)> = rows(&l, Some(day()))
        .iter()
        .map(|r| (r.get("job").and_then(Value::as_str).map(String::from), r["usd"].as_f64().unwrap()))
        .collect();
    let c = |s: &str| Some(s.to_string());
    assert_eq!(got, vec![(None, 0.001), (c("c7"), 0.002), (c("c8"), 0.003), (c("c7"), 0.004), (None, 0.005)]);
    let text = std::fs::read_to_string(tmp.path().join("2026-09-24.jsonl")).unwrap();
    let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert!(first.get("job").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_job_tag_survives_asyncio_to_thread_and_is_gone_after() {
    let (_tmp, l) = ledger();
    let when = noon(day());
    let l1 = l.clone();
    in_job("c7", async move {
        tokio::task::yield_now().await;
        to_thread(move || l1.record("openrouter", "m", CHIEF, Some(0.01), RecordOpts::new().now(when))).await;
    })
    .await;
    let l2 = l.clone();
    to_thread(move || l2.record("openrouter", "m", CHIEF, Some(0.02), RecordOpts::new().now(when))).await;
    let jobs: Vec<Option<Value>> = rows(&l, Some(day())).iter().map(|r| r.get("job").cloned()).collect();
    assert_eq!(jobs, vec![Some(json!("c7")), None]);
}

#[test]
fn test_a_job_tag_survives_the_search_routers_worker_thread() {
    // search_router.bounded copies the context into its worker; `carry` is that copy.
    let (_tmp, l) = ledger();
    let when = noon(day());
    {
        let _g = job("c9");
        let l1 = l.clone();
        let work = carry(move || {
            l1.record("openrouter", "m", CHIEF, Some(0.01), RecordOpts::new().now(when));
            json!({"ok": true})
        });
        assert_eq!(std::thread::spawn(work).join().unwrap(), json!({"ok": true}));
    }
    // positive control: a bare thread started inside the job loses the tag, which is why bounded copies it
    {
        let _g = job("c9");
        let l2 = l.clone();
        std::thread::spawn(move || l2.record("openrouter", "m", CHIEF, Some(0.02), RecordOpts::new().now(when)))
            .join()
            .unwrap();
    }
    let jobs: Vec<Option<Value>> = rows(&l, Some(day())).iter().map(|r| r.get("job").cloned()).collect();
    assert_eq!(jobs, vec![Some(json!("c9")), None]);
}

#[test]
fn test_a_bad_card_id_tags_nothing_and_job_total_sums_one_cards_rows() {
    let (tmp, l) = ledger();
    let d = day();
    let next = d + Duration::days(1);
    {
        let _g = job("c7; rm -rf ~");
        l.record("openrouter", "m", CHIEF, Some(0.5), RecordOpts::new().now(noon(d)));
    }
    {
        let _g = job("c7");
        l.record("openrouter", "m", CHIEF, Some(0.25), RecordOpts::new().now(noon(d)));
        l.record("openrouter", "m", CHIEF, None, RecordOpts::new().now(noon(d))); // unpriced: counts nothing
        l.record("openrouter", "m", CHIEF, Some(0.125), RecordOpts::new().now(noon(next)));
    }
    {
        let _g = job("c70");
        l.record("openrouter", "m", CHIEF, Some(1.0), RecordOpts::new().now(noon(d)));
    }
    assert!(rows(&l, Some(d))[0].get("job").is_none());
    assert_eq!(job_total("c7", d, Some(next), Some(tmp.path())), 0.375);
    assert_eq!(job_total("c7", next, Some(next), Some(tmp.path())), 0.125);
    assert_eq!(
        totals(rows(&l, Some(d))).by_feature,
        IndexMap::from([("chief of staff".to_string(), 1.75)])
    );
}

#[test]
fn a_card_id_with_a_trailing_newline_is_kept_as_python_does() {
    assert_eq!(clean_card_id("c7\n"), "c7\n");
    assert_eq!(clean_card_id("c0"), "");
    assert_eq!(clean_card_id("c1234567890"), "");
    assert_eq!(clean_card_id("c123456789"), "c123456789");
}

#[test]
fn record_lines_are_byte_identical_to_the_python_writer() {
    // The expected text is what spend.py wrote for the same calls (run 2026-10-19).
    let (tmp, l) = ledger();
    let at = 1_790_000_000.0;
    l.record("", &"x".repeat(100), "", Some(0.000012345678912), RecordOpts::new().note("  a\n\tb  é ").now(at));
    {
        let _g = job("c7");
        l.record(
            "openai",
            "gpt-6-luna",
            "chat",
            Some(0.00042),
            RecordOpts::new().tokens([("in", json!(3100)), ("cached", json!(0)), ("out", json!(80))]).now(at).source("reported"),
        );
    }
    l.record("openai", "m", "voice", Some(1e16), RecordOpts::new().now(at));
    let day = local_day(at).unwrap().format("%Y-%m-%d").to_string();
    let text = std::fs::read_to_string(tmp.path().join(format!("{day}.jsonl"))).unwrap();
    let want = concat!(
        r#"{"t":1790000000,"p":"?","m":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx","f":"other","usd":1.235e-05,"src":"priced","note":"a b \u00e9"}"#, "\n",
        r#"{"t":1790000000,"p":"openai","m":"gpt-6-luna","f":"chat","usd":0.00042,"src":"reported","tok":{"in":3100,"out":80},"job":"c7"}"#, "\n",
        r#"{"t":1790000000,"p":"openai","m":"m","f":"voice","usd":1e+16,"src":"priced"}"#, "\n",
    );
    assert_eq!(text, want);
}
