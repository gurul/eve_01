//! Replays `fixtures/python_parity.json` (written by `fixtures/gen_parity.py` from the Python gate) through
//! the Rust gate: every returned block, every decision, every stat and the centroid, bit for bit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use eve_voice_gate::*;
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/python_parity.json");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// A float the Python wrote as `repr()`: Rust's parse is correctly rounded, so this is the same bits.
fn pf(v: &Value) -> Option<f64> {
    v.as_str().map(|s| s.parse::<f64>().unwrap())
}

struct FakeEmbedder {
    vec: Arc<HashMap<i16, Vec<f64>>>,
    fail: bool,
}

impl Embedder for FakeEmbedder {
    fn embed(&mut self, pcm: &[u8]) -> Result<Option<Vec<f64>>, GateError> {
        if self.fail {
            return Err(GateError::new("RuntimeError", "onnx went away"));
        }
        let mut total = vec![0.0f64; 3];
        for c in pcm.chunks_exact(2) {
            if let Some(v) = self.vec.get(&i16::from_le_bytes([c[0], c[1]])) {
                for (t, x) in total.iter_mut().zip(v) {
                    *t += x;
                }
            }
        }
        Ok(if total.iter().any(|&x| x != 0.0) { Some(total) } else { None })
    }
}

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::SeqCst))
    }
    fn advance(&self, by: f64) {
        self.0.store((self.get() + by).to_bits(), Ordering::SeqCst);
    }
}

fn config(mode: &str, tweaks: &serde_json::Map<String, Value>) -> GateConfig {
    let mut c = GateConfig { speech_rms: 0.01, ..GateConfig::with_mode(mode) };
    for (k, v) in tweaks {
        match k.as_str() {
            "open_after_rejected_secs" => c.open_after_rejected_secs = pf(v).unwrap(),
            "enrol_min_secs" => c.enrol_min_secs = pf(v).unwrap(),
            "first_turn_secs" => c.first_turn_secs = pf(v).unwrap(),
            "enrol_cap_secs" => c.enrol_cap_secs = pf(v).unwrap(),
            "hang_blocks" => c.hang_blocks = v.as_u64().unwrap() as u32,
            other => panic!("unknown tweak {other}"),
        }
    }
    c
}

#[test]
fn random_scenarios_match_the_python_gate_block_for_block() {
    let data: Value = serde_json::from_str(FIXTURE).unwrap();
    let block = data["block"].as_u64().unwrap() as usize;
    let vocab: Vec<Vec<u8>> = data["vocab"].as_array().unwrap().iter().map(|v| unhex(v.as_str().unwrap())).collect();
    let vec: Arc<HashMap<i16, Vec<f64>>> = Arc::new(
        data["vec"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()))
            .collect(),
    );
    let scenarios = data["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 400);
    for sc in scenarios {
        let seed = &sc["seed"];
        let tweaks = sc["tweaks"].as_object().unwrap();
        let clock = TestClock(Arc::new(AtomicU64::new(0f64.to_bits())));
        let c2 = clock.clone();
        let embedder: Option<Box<dyn Embedder>> = if sc["no_embedder"].as_bool().unwrap() {
            None
        } else {
            Some(Box::new(FakeEmbedder { vec: vec.clone(), fail: false }))
        };
        let mut g = SpeakerGate::with_clock(
            config(sc["mode"].as_str().unwrap(), tweaks),
            embedder,
            Box::new(move || c2.get()),
            block * 2,
        );
        let wake: Vec<u8> =
            sc["wake"].as_array().unwrap().iter().flat_map(|i| vocab[i.as_u64().unwrap() as usize].clone()).collect();
        assert_eq!(g.enrol_seed(&wake).unwrap(), sc["enrolled"].as_bool().unwrap(), "seed {seed}: enrol");
        let outputs = sc["outputs"].as_array().unwrap();
        for (n, (op, want)) in sc["ops"].as_array().unwrap().iter().zip(outputs).enumerate() {
            match op["op"].as_str().unwrap() {
                "feed" => {
                    let want = want.as_array().unwrap();
                    for (j, (b, w)) in op["blocks"].as_array().unwrap().iter().zip(want).enumerate() {
                        clock.advance(BLOCK_SECS);
                        let got = g.process(&vocab[b.as_u64().unwrap() as usize]);
                        let w: Vec<Vec<u8>> =
                            w.as_array().unwrap().iter().map(|i| vocab[i.as_u64().unwrap() as usize].clone()).collect();
                        assert_eq!(got, w, "seed {seed}: op {n} block {j}");
                    }
                }
                "lenient" => g.lenient = op["value"].as_bool().unwrap(),
                "bypass" => g.bypass = op["value"].as_bool().unwrap(),
                "advance" => clock.advance(pf(&op["secs"]).unwrap()),
                "fail" => g.embedder = Some(Box::new(FakeEmbedder { vec: vec.clone(), fail: true })),
                "reset" => g.reset(),
                "mode" => g.config = config(op["value"].as_str().unwrap(), tweaks),
                other => panic!("unknown op {other}"),
            }
        }
        assert_eq!(g.state(), sc["state"].as_str().unwrap(), "seed {seed}: state");
        let st = &sc["stats"];
        let s = &g.stats;
        assert_eq!(
            (s.segments, s.accepted, s.rejected, s.opened.as_str()),
            (
                st["segments"].as_u64().unwrap(),
                st["accepted"].as_u64().unwrap(),
                st["rejected"].as_u64().unwrap(),
                st["opened"].as_str().unwrap()
            ),
            "seed {seed}: counts"
        );
        assert_eq!(s.secs_forwarded, pf(&st["secs_forwarded"]).unwrap(), "seed {seed}: forwarded");
        assert_eq!(s.secs_silenced, pf(&st["secs_silenced"]).unwrap(), "seed {seed}: silenced");
        assert_eq!(s.enrolled_secs, pf(&st["enrolled_secs"]).unwrap(), "seed {seed}: enrolled");
        let summary = g.summary();
        for (k, v) in sc["summary"].as_object().unwrap() {
            match v {
                Value::String(x) if k.starts_with("secs_") || k == "enrolled_secs" => {
                    assert_eq!(summary[k].as_f64().unwrap(), x.parse::<f64>().unwrap(), "seed {seed}: summary {k}")
                }
                _ => assert_eq!(&summary[k], v, "seed {seed}: summary {k}"),
            }
        }
        let want: Vec<&Value> = sc["decisions"].as_array().unwrap().iter().collect();
        assert_eq!(s.decisions.len(), want.len(), "seed {seed}: decision count");
        for (i, (d, w)) in s.decisions.iter().zip(want).enumerate() {
            let w = w.as_array().unwrap();
            assert_eq!(d.at, pf(&w[0]).unwrap(), "seed {seed}: decision {i} at");
            assert_eq!(d.secs, pf(&w[1]).unwrap(), "seed {seed}: decision {i} secs");
            assert_eq!(d.state, w[2].as_str().unwrap(), "seed {seed}: decision {i} state");
            assert_eq!(d.score, pf(&w[3]), "seed {seed}: decision {i} score");
            assert_eq!(d.threshold, pf(&w[4]).unwrap(), "seed {seed}: decision {i} threshold");
            assert_eq!(d.accepted, w[5].as_bool().unwrap(), "seed {seed}: decision {i} accepted");
            assert_eq!(d.why, w[6].as_str().unwrap(), "seed {seed}: decision {i} why");
        }
    }
}

#[test]
fn arithmetic_matches_python() {
    let data: Value = serde_json::from_str(FIXTURE).unwrap();
    let a = &data["arithmetic"];
    for c in a["rms"].as_array().unwrap() {
        assert_eq!(rms(&unhex(c[0].as_str().unwrap())), pf(&c[1]).unwrap(), "rms {}", c[0]);
    }
    for c in a["cosine"].as_array().unwrap() {
        let x: Vec<f64> = c[0].as_array().unwrap().iter().map(|v| pf(v).unwrap()).collect();
        let y: Vec<f64> = c[1].as_array().unwrap().iter().map(|v| pf(v).unwrap()).collect();
        assert_eq!(cosine(&x, &y).unwrap(), pf(&c[2]).unwrap(), "cosine {x:?} {y:?}");
    }
    for c in a["trim"].as_array().unwrap() {
        let got = trim_to_speech(&unhex(c[0].as_str().unwrap()), pf(&c[1]).unwrap(), c[2].as_u64().unwrap() as usize);
        assert_eq!(got, unhex(c[3].as_str().unwrap()), "trim {c}");
    }
    for c in a["ring"].as_array().unwrap() {
        let mut ring = WakeRing::new(pf(&c[0]).unwrap());
        for i in 0..60u8 {
            ring.add(&[i]);
        }
        assert_eq!(ring.snapshot(), unhex(c[1].as_str().unwrap()), "ring {}", c[0]);
    }
    for c in a["env"].as_array().unwrap() {
        let mode = c[0].as_str().map(str::to_string);
        let model = c[1].as_str().map(str::to_string);
        let got = configured_from(|k| match k {
            "EVE_VOICE_GATE" => mode.clone(),
            "EVE_VOICE_GATE_MODEL" => model.clone(),
            _ => None,
        });
        assert_eq!((got.mode.as_str(), got.model.as_str()), (c[2].as_str().unwrap(), c[3].as_str().unwrap()), "env {c}");
    }
}
