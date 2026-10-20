//! Port of `bridge/tests/test_protocol.py` and the protocol half of
//! `bridge/tests/test_agent_rows.py`.

use super::*;
use serde_json::json;

fn hb(s: &State) -> Map<String, Value> {
    build_heartbeat(s, None, None).unwrap()
}

fn hb_codec(s: &State, codec: Option<&str>) -> Map<String, Value> {
    build_heartbeat(s, None, codec).unwrap()
}

fn entry(snap: &Map<String, Value>, i: usize) -> &str {
    snap["entries"][i].as_str().unwrap()
}

fn enc(v: Value) -> Vec<u8> {
    encode(&v, None).unwrap()
}

fn enc_codec(v: Value, codec: &str) -> Vec<u8> {
    encode(&v, Some(codec)).unwrap()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// ---- test_protocol.py ----

#[test]
fn test_heartbeat_empty_state() {
    let snap = hb(&State::new());
    assert_eq!(snap["total"], 0);
    assert_eq!(snap["running"], 0);
    assert_eq!(snap["waiting"], 0);
    assert_eq!(snap["entries"], json!([]));
    assert_eq!(snap["tokens"], 0);
    assert!(!snap.contains_key("prompt"));
}

#[test]
fn test_heartbeat_omits_completed_when_idle() {
    let s = State::new();
    assert!(!hb(&s).contains_key("completed"));
}

#[test]
fn test_heartbeat_includes_completed_during_pulse() {
    let mut s = State::new();
    s.pulse_completed(2.0);
    assert_eq!(hb(&s).get("completed"), Some(&Value::Bool(true)));
}

#[test]
fn test_heartbeat_drops_completed_after_pulse_expires() {
    let mut s = State::new();
    s.pulse_completed(-1.0); // already expired
    assert!(!hb(&s).contains_key("completed"));
}

#[test]
fn test_heartbeat_entries_formatted() {
    let mut s = State::new();
    s.add_entry("hello world", Some(0.0)); // epoch 0 → local HH:MM
    let snap = hb(&s);
    assert_eq!(snap["entries"].as_array().unwrap().len(), 1);
    // Should be "HH:MM hello world" — just check suffix since HH:MM is tz-local.
    assert!(entry(&snap, 0).ends_with(" hello world"));
}

/// State keeps newest-first for cheap prepend; the wire format is reversed
/// so the firmware's `lines[n-1]=newest` assumption holds.
#[test]
fn test_heartbeat_entries_on_wire_are_oldest_first() {
    let mut s = State::new();
    s.add_entry("oldest", Some(0.0));
    s.add_entry("middle", Some(0.0));
    s.add_entry("newest", Some(0.0));
    let snap = hb(&s);
    // On wire: oldest → middle → newest
    assert!(entry(&snap, 0).ends_with(" oldest"));
    assert!(entry(&snap, 1).ends_with(" middle"));
    assert!(entry(&snap, 2).ends_with(" newest"));
}

#[test]
fn test_encode_terminates_with_newline() {
    assert!(enc(json!({"a": 1})).ends_with(b"\n"));
}

#[test]
fn test_line_assembler_fragments() {
    let mut la = LineAssembler::new();
    assert_eq!(la.feed(br#"{"a":1}
{"b":"#), vec![json!({"a": 1})]);
    assert_eq!(la.feed(b"2}\n"), vec![json!({"b": 2})]);
}

#[test]
fn test_line_assembler_drops_bad_lines() {
    let mut la = LineAssembler::new();
    assert_eq!(la.feed(b"garbage\n{\"ok\":true}\n"), vec![json!({"ok": true})]);
}

#[test]
fn test_line_assembler_empty_lines_ignored() {
    let mut la = LineAssembler::new();
    assert!(la.feed(b"\n\n\n").is_empty());
}

// ---- sanitize_for_stick ----

#[test]
fn test_sanitize_keeps_ascii() {
    assert_eq!(sanitize_for_stick("hello world 123 !@#", None), "hello world 123 !@#");
}

#[test]
fn test_sanitize_strips_cjk() {
    // The stock firmware does not enable HZK16. Letting CJK through crashes
    // the BLE link on the stick — see sanitize_for_stick's doc comment.
    let out = sanitize_for_stick("hello 你好 world", None);
    assert!(!out.contains('你') && !out.contains('好'));
    assert!(out.contains("hello") && out.contains("world"));
}

#[test]
fn test_sanitize_strips_bmp_symbols() {
    // Same firmware-glyph-table issue as CJK.
    let out = sanitize_for_stick("› done ✓", None);
    assert!(!out.contains('›'));
    assert!(!out.contains('✓'));
    assert!(out.contains("done"));
}

#[test]
fn test_sanitize_strips_emoji() {
    // 🎮 is U+1F3AE (supplementary plane).
    let out = sanitize_for_stick("press A 🎮 now", None);
    assert!(!out.contains('🎮'));
    assert!(out.contains("press A") && out.contains("now"));
}

#[test]
fn test_sanitize_strips_multiple_emojis() {
    let out = sanitize_for_stick("🐾🎮🔴hello🌙", None);
    assert!(!out.chars().any(|c| c as u32 >= 0x10000));
    assert!(out.contains("hello"));
}

#[test]
fn test_sanitize_strips_newlines_and_control_chars() {
    assert!(!sanitize_for_stick("hello\nworld", None).contains('\n'));
    assert!(!sanitize_for_stick("hello\x00world", None).contains('\x00'));
    assert!(!sanitize_for_stick("hello\x7fworld", None).contains('\x7f'));
    assert!(!sanitize_for_stick("hello\u{85}world", None).contains('\u{85}'));
}

#[test]
fn test_sanitize_preserves_tab() {
    assert_eq!(sanitize_for_stick("a\tb", None), "a\tb");
}

#[test]
fn test_sanitize_empty_string() {
    assert_eq!(sanitize_for_stick("", None), "");
}

#[test]
fn test_heartbeat_sanitizes_entries() {
    let mut s = State::new();
    s.add_entry("got 🐾 paw", None);
    assert!(!entry(&hb(&s), 0).contains('🐾'));
}

// ---- codec-aware encoding (CJK firmware variant) ----

/// Spot-check the codec map: zh-CN → gbk, zh-TW → big5, ja → shift_jis.
#[test]
fn test_cjk_codecs_mapping() {
    assert_eq!(cjk_codec("zh-CN"), Some("gbk"));
    assert_eq!(cjk_codec("zh-TW"), Some("big5"));
    assert_eq!(cjk_codec("ja"), Some("shift_jis"));
}

/// With codec='gbk', characters representable in GBK pass through.
#[test]
fn test_sanitize_for_stick_gbk_passes_cjk() {
    let out = sanitize_for_stick("hello 你好 world", Some("gbk"));
    assert!(out.contains('你') && out.contains('好'));
    assert_eq!(out, "hello 你好 world");
}

/// Emoji are supplementary-plane, not in GBK — replaced with '?'.
#[test]
fn test_sanitize_for_stick_gbk_strips_emoji() {
    let out = sanitize_for_stick("press 🎮 now", Some("gbk"));
    assert!(!out.contains('🎮'));
    assert!(out.contains("press") && out.contains("now"));
}

/// Full-width punctuation (zone 1) is in GBK — must pass through.
#[test]
fn test_sanitize_for_stick_gbk_keeps_punctuation() {
    let out = sanitize_for_stick("这是、一个，测试。", Some("gbk"));
    for ch in "这是、一个，测试。".chars() {
        assert!(out.contains(ch), "{ch}");
    }
}

/// Hiragana/katakana are in JIS X 0208.
#[test]
fn test_sanitize_for_stick_shift_jis_passes_kana() {
    let out = sanitize_for_stick("こんにちは Tokyo タワー", Some("shift_jis"));
    assert!(out.contains("こんにちは"));
    assert!(out.contains("タワー"));
}

/// Unknown codec name → ASCII-only fallback (defensive).
#[test]
fn test_sanitize_for_stick_unknown_codec_falls_back() {
    assert!(!sanitize_for_stick("hello 你好", Some("not_a_codec")).contains('你'));
}

/// Default (codec=None) preserves the ASCII-only safety policy.
#[test]
fn test_sanitize_for_stick_none_codec_strips_cjk() {
    let out = sanitize_for_stick("hello 你好", None);
    assert!(!out.contains('你') && !out.contains('好'));
}

/// encode() with no codec arg produces the same UTF-8 JSON as before.
#[test]
fn test_encode_default_codec_unchanged() {
    assert_eq!(enc(json!({"a": 1, "msg": "hello"})), b"{\"a\":1,\"msg\":\"hello\"}\n".to_vec());
}

/// encode() with codec='gbk' encodes string values as GBK bytes inline.
#[test]
fn test_encode_gbk_codec_emits_gbk_bytes() {
    let out = enc_codec(json!({"msg": "你好"}), "gbk");
    // 你 GBK = C4 E3, 好 GBK = BA C3
    assert!(contains(&out, b"\"msg\":\"\xC4\xE3\xBA\xC3\""));
    assert!(out.ends_with(b"\n"));
}

/// Keys and structural JSON stay ASCII; only string *values* switch codec.
#[test]
fn test_encode_gbk_keeps_ascii_structure() {
    let out = enc_codec(json!({"total": 5, "msg": "好", "entries": ["A", "好"]}), "gbk");
    // Keys are plain ASCII even under gbk encoding.
    assert!(contains(&out, b"\"total\":5"));
    assert!(contains(&out, b"\"msg\":\"\xBA\xC3\""));
    assert!(contains(&out, b"\"entries\":[\"A\",\"\xBA\xC3\"]"));
}

/// Backslash, double-quote, control chars still get JSON-escaped under GBK.
#[test]
fn test_encode_codec_escapes_json_metachars() {
    let out = enc_codec(json!({"msg": "has \"quote\" and \\back"}), "gbk");
    assert!(contains(&out, b"\\\"quote\\\""));
    assert!(contains(&out, b"\\\\back"));
}

/// Emoji can't go through GBK — replaced with '?' via errors='replace'.
#[test]
fn test_encode_gbk_drops_unencodable() {
    let out = enc_codec(json!({"msg": "你🎮好"}), "gbk");
    // Emoji becomes '?' (the GBK codec's replacement byte).
    assert!(contains(&out, b"?"));
    assert!(contains(&out, b"\"msg\":\"\xC4\xE3?\xBA\xC3\""));
}

/// When build_heartbeat is called with a codec, entries keep their CJK.
#[test]
fn test_build_heartbeat_threads_codec_into_sanitize() {
    let mut s = State::new();
    s.add_entry("@ 你好世界", None);
    let snap = hb_codec(&s, Some("gbk"));
    // The "@ ..." entry survived sanitization with the codec set.
    assert!(entry(&snap, 0).contains("你好世界"));
}

/// Sanity check: same heartbeat without codec ends up '?'-padded.
#[test]
fn test_build_heartbeat_no_codec_strips_cjk() {
    let mut s = State::new();
    s.add_entry("@ 你好世界", None);
    let snap = hb_codec(&s, None);
    assert!(!entry(&snap, 0).contains("你好世界"));
    assert!(entry(&snap, 0).contains('?'));
}

// ---- test_agent_rows.py (the build_heartbeat half) ----

#[test]
fn test_heartbeat_carries_agents_and_stays_small() {
    let mut st = State::new();
    for i in 0..State::MAX_AGENT_ROWS {
        let cwd = format!("/tmp/some-repo-{i}");
        st.session_start(&format!("s{i}"), None, Some(&cwd));
        st.note_tool(&format!("s{i}"), "ToolName");
    }
    let snap = hb(&st);
    let agents = snap["agents"].as_array().unwrap();
    assert_eq!(agents.len(), State::MAX_AGENT_ROWS);
    let keys: Vec<&str> = agents[0].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["n", "s", "t"]);
    // The firmware reads heartbeats into a 2KB line buffer shared with a
    // 720-byte prompt detail; a full agent table must not crowd it out.
    assert!(enc(Value::Object(snap)).len() - 1 < 1200);
}

/// Present-but-empty is how 'all sessions ended' reaches the firmware —
/// an absent key leaves the previous list standing on the panel.
#[test]
fn test_empty_agents_key_is_always_present() {
    assert_eq!(hb(&State::new())["agents"], json!([]));
}

// ---- beyond the Python suite ----

#[test]
fn heartbeat_key_order_matches_python() {
    let mut s = State::new();
    s.pulse_completed(5.0);
    let keys: Vec<String> = hb(&s).keys().cloned().collect();
    assert_eq!(
        keys,
        ["total", "running", "waiting", "msg", "entries", "tokens", "tokens_today", "agents", "completed"]
    );
}

#[test]
fn default_msg_counts() {
    let mut s = State::new();
    assert_eq!(hb(&s)["msg"], "no sessions");
    s.session_start("a", None, None);
    s.session_start("b", None, None);
    assert_eq!(hb(&s)["msg"], "2 idle");
    s.turn_begin("a");
    assert_eq!(hb(&s)["msg"], "1 running");
    let snap = build_heartbeat(&s, Some("custom ✓"), None).unwrap();
    assert_eq!(snap["msg"], "custom ?");
}

#[test]
fn agent_rows_are_sanitized() {
    let mut s = State::new();
    s.session_start("x", None, Some("/tmp/仓库"));
    let snap = hb(&s);
    assert_eq!(snap["agents"][0]["n"], "??");
    let snap = hb_codec(&s, Some("gbk"));
    assert_eq!(snap["agents"][0]["n"], "仓库");
}

#[test]
fn format_entry_flattens_strips_and_truncates() {
    let e = format_entry(0.0, "  line one\nline two  ").unwrap();
    assert_eq!(&e[2..3], ":");
    assert_eq!(&e[5..], " line one line two");
    let long = "x".repeat(100);
    let e = format_entry(0.0, &long).unwrap();
    assert_eq!(&e[6..], format!("{}…", "x".repeat(57)));
}

#[test]
fn format_entry_timestamp_errors_match_python() {
    assert_eq!(
        format_entry(f64::NAN, "x").unwrap_err().to_string(),
        "Invalid value NaN (not a number)"
    );
    assert_eq!(
        format_entry(f64::INFINITY, "x").unwrap_err().to_string(),
        "timestamp out of range for platform time_t"
    );
    assert_eq!(format_entry(1e20, "x").unwrap_err().to_string(), "timestamp out of range for platform time_t");
    let e = format_entry(1e12, "x").unwrap_err();
    assert!(matches!(e, TimestampError::Value(ref m) if m.starts_with("year 3365") && m.ends_with(" is out of range")));
}

#[test]
fn format_entry_rounds_to_microseconds_like_python() {
    // datetime.fromtimestamp(59.9999996) is 00:01:00 UTC-local, not 00:00:59.
    assert_eq!(local_hhmm(59.9999996).unwrap(), local_hhmm(60.0).unwrap());
    assert_eq!(local_hhmm(-0.5).unwrap(), local_hhmm(-1.0).unwrap());
}

#[test]
fn truncate_utf8_bytes_keeps_codepoints_whole() {
    assert_eq!(truncate_utf8_bytes("short", 60), "short");
    assert_eq!(truncate_utf8_bytes("abcdef", 3), "…");
    assert_eq!(truncate_utf8_bytes("abcdef", 5), "ab…");
    // 你 is 3 bytes: a budget of 4 (7 - 3) holds one of them plus nothing of the next.
    assert_eq!(truncate_utf8_bytes("你好世界", 7), "你…");
    assert_eq!(truncate_utf8_bytes("你好世界", 9), "你好…");
    assert_eq!(truncate_utf8_bytes("a你好", 4), "a…");
}

#[test]
fn encode_utf8_matches_json_dumps() {
    let v = json!({
        "a": 1e16, "b": 1e-5, "c": 0.1, "d": 1.0, "e": 123456789012345678.0_f64,
        "f": -0.0, "g": 1.5e300, "h": "\u{7f}\u{0c}\u{01}é\"\\\n", "i": [true, false, null], "j": -7,
        "k": 18446744073709551615u64, "l": 0.0001, "m": 1e15,
    });
    let out = String::from_utf8(enc(v)).unwrap();
    assert_eq!(
        out,
        "{\"a\":1e+16,\"b\":1e-05,\"c\":0.1,\"d\":1.0,\"e\":1.2345678901234568e+17,\"f\":-0.0,\"g\":1.5e+300,\
         \"h\":\"\u{7f}\\f\\u0001é\\\"\\\\\\n\",\"i\":[true,false,null],\"j\":-7,\"k\":18446744073709551615,\
         \"l\":0.0001,\"m\":1000000000000000.0}\n"
    );
}

#[test]
fn encode_codec_escapes_controls_as_python() {
    let out = enc_codec(json!({"m": "\u{0c}\u{08}\t\r\n\u{1f}", "f": 2.5}), "gbk");
    assert_eq!(out, b"{\"m\":\"\\u000c\\b\\t\\r\\n\\u001f\",\"f\":2.5}\n".to_vec());
}

#[test]
fn encode_codec_keys_are_ascii_with_replacement() {
    let out = enc_codec(json!({"ké": "é"}), "gbk");
    // The key goes out as ASCII ('?'), the value in gbk (A8 A6).
    assert_eq!(out, b"{\"k?\":\"\xA8\xA6\"}\n".to_vec());
}

#[test]
fn encode_codec_escapes_trail_byte_backslash() {
    // A shift_jis character whose second byte is 0x5C ('ソ' is 0x83 0x5C)
    // gets that byte escaped, as Python's byte-level escaping does.
    let out = enc_codec(json!({"m": "ソ"}), "shift_jis");
    assert_eq!(out, b"{\"m\":\"\x83\\\\\"}\n".to_vec());
}

#[test]
fn encode_unknown_codec_errors_only_on_strings() {
    assert_eq!(encode(&json!({"a": 1}), Some("bogus")).unwrap(), b"{\"a\":1}\n".to_vec());
    let err = encode(&json!({"a": "x"}), Some("bogus")).unwrap_err();
    assert_eq!(err.to_string(), "unknown encoding: bogus");
}

#[test]
fn encode_utf8_alias_takes_the_codec_path() {
    // Only the exact "utf-8" means json.dumps; "UTF-8" goes byte by byte.
    let out = enc_codec(json!({"m": "é\u{0c}"}), "UTF-8");
    assert_eq!(out, "{\"m\":\"é\\u000c\"}\n".as_bytes().to_vec());
    assert_eq!(sanitize_for_stick("é🎮", Some("utf8")), "é🎮");
}

#[test]
fn sanitize_shift_jis_reads_yen_back_as_backslash() {
    assert_eq!(sanitize_for_stick("¥100", Some("shift_jis")), "\\100");
}

#[test]
fn line_assembler_strips_ascii_whitespace_and_keeps_scalars() {
    let mut la = LineAssembler::new();
    let out = la.feed(b"  {\"a\":1}\r\n\x0b5\x0c\n[1]\n\xff\xfe\n\"s\"\n");
    assert_eq!(out, vec![json!({"a": 1}), json!(5), json!([1]), json!("s")]);
    // A partial line stays buffered.
    assert!(la.feed(b"{\"x\"").is_empty());
    assert_eq!(la.feed(b":2}\n"), vec![json!({"x": 2})]);
}

#[test]
fn time_sync_shape() {
    let m = build_time_sync();
    let arr = m["time"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let now = arr[0].as_i64().unwrap();
    assert!((now as f64 - eve_state::wall_time()).abs() < 5.0);
    assert!(arr[1].as_i64().unwrap().abs() <= 18 * 3600);
}

#[test]
fn constants_match_python() {
    assert_eq!(NUS_RX_UUID, "6e400002-b5a3-f393-e0a9-e50e24dcca9e");
    assert_eq!(NUS_TX_UUID, "6e400003-b5a3-f393-e0a9-e50e24dcca9e");
    assert_eq!(HEARTBEAT_KEEPALIVE, 10.0);
    assert_eq!(ENTRY_MAX_BYTES, 60);
    assert_eq!(UNRENDERABLE_REPLACEMENT, "?");
    assert_eq!(cjk_codec("en"), None);
}

/// Runs the Python module on the same inputs and compares bytes: sanitize and
/// encode in every regime, entries and float formatting.
#[test]
#[ignore = "live: needs the bridge's Python (EVE_PROTOCOL_PYTHON or python3)"]
fn differential_against_python_protocol() {
    let python = std::env::var("EVE_PROTOCOL_PYTHON").unwrap_or_else(|_| "python3".into());
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src");
    // Deterministic sample: ASCII, controls, Latin-1, Greek, kana, CJK, full-width, emoji.
    let mut seed: u64 = 0x5eed;
    let pools: [(u32, u32); 9] = [
        (0x00, 0x7f), (0x80, 0xff), (0x370, 0x3ff), (0x2000, 0x206f), (0x3000, 0x30ff),
        (0x4e00, 0x9fff), (0xff00, 0xffef), (0x1f300, 0x1f6ff), (0x2190, 0x22ff),
    ];
    let mut texts = Vec::new();
    for _ in 0..400 {
        let mut t = String::new();
        for _ in 0..12 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let (lo, hi) = pools[(seed >> 33) as usize % pools.len()];
            let cp = lo + ((seed >> 13) as u32) % (hi - lo + 1);
            if let Some(c) = char::from_u32(cp) {
                t.push(c);
            }
        }
        texts.push(t);
    }
    let floats = [0.1, 1e16, 1e-5, 2.5, -3.75e-7, 123456.789, 1e22, 5e-324, 1.7976931348623157e308];
    let input = json!({"texts": texts, "floats": floats});
    let script = r#"
import json, sys
from eve_bridge.protocol import encode, sanitize_for_stick, truncate_utf8_bytes
d = json.load(sys.stdin)
out = []
for codec in [None, "gbk", "big5", "shift_jis", "UTF8", "bogus"]:
    for t in d["texts"]:
        out.append(sanitize_for_stick(t, codec))
        try:
            out.append(encode({"k" + t: t, "n": d["floats"]}, codec).hex())
        except LookupError as e:
            out.append("LookupError:" + str(e))
for t in d["texts"]:
    out.append(truncate_utf8_bytes(t, 20))
json.dump(out, sys.stdout)
"#;
    let mut child = std::process::Command::new(python)
        .args(["-c", script])
        .current_dir(src)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let want: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    let mut got = Vec::new();
    for codec in [None, Some("gbk"), Some("big5"), Some("shift_jis"), Some("UTF8"), Some("bogus")] {
        for t in &texts {
            got.push(sanitize_for_stick(t, codec));
            let mut m = Map::new();
            m.insert(format!("k{t}"), Value::String(t.clone()));
            m.insert("n".into(), json!(floats));
            got.push(match encode(&Value::Object(m), codec) {
                Ok(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
                Err(e) => format!("LookupError:{e}"),
            });
        }
    }
    for t in &texts {
        got.push(truncate_utf8_bytes(t, 20));
    }
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "case {i}");
    }
}
