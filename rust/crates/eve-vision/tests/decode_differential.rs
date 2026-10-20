//! decode_frame against CPython 3.13: fixtures/decode_frame.json holds what
//! the Python decode_frame returned or raised for each input
//! (regenerate with fixtures/gen_decode.py).

use base64::Engine as _;
use eve_vision::decode_frame;
use serde_json::Value;

#[test]
fn test_decode_frame_matches_python_cases() {
    let raw = include_str!("fixtures/decode_frame.json");
    let cases: Vec<Value> = serde_json::from_str(raw).unwrap();
    assert!(cases.len() > 100);
    let mut failures = Vec::new();
    for case in &cases {
        let input = &case["in"];
        // seq=1e300: Python builds a big int; Rust rejects values beyond i64 (PARITY.md, Differences).
        if input.get("seq").and_then(Value::as_f64).is_some_and(|f| f.abs() > 9.0e18) {
            assert!(decode_frame(input).is_err());
            continue;
        }
        let got = decode_frame(input);
        match (case.get("ok"), case.get("err"), got) {
            (Some(ok), None, Ok(f)) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&f.data);
                let want = (
                    ok["seq"].as_i64(),
                    ok["w"].as_i64(),
                    ok["h"].as_i64(),
                    ok["fmt"].as_str(),
                    ok["data"].as_str(),
                    ok["yaw"].as_f64(),
                    ok["pitch"].as_f64(),
                    ok["snap"].as_bool(),
                );
                let have = (
                    Some(f.seq),
                    Some(f.w),
                    Some(f.h),
                    Some(f.fmt.as_str()),
                    Some(data.as_str()),
                    f.yaw,
                    f.pitch,
                    Some(f.snap),
                );
                if want != have {
                    failures.push(format!("{input}: want {want:?}, have {have:?}"));
                }
            }
            (None, Some(err), Err(e)) => {
                if err.as_str() != Some(e.0.as_str()) {
                    failures.push(format!("{input}: want error {err}, have {:?}", e.0));
                }
            }
            (_, _, got) => failures.push(format!("{input}: want {case}, have {got:?}")),
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}
