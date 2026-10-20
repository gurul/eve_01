//! lite_rtc end to end in one process: two real WebRTC peers, the web app's side played by one (`Page`), the Mac's
//! by RtcHub with the unchanged LinkChat and PhoneCalls behind it (port of bridge/tests/test_lite_rtc_loopback.py).
//! A turn completes over the `link` channel; a 1 kHz tone held on the page's microphone reaches the call's press;
//! eve's answer comes back as sound on the audio track with its caption. Then the hub: no ICE server at all, the
//! `.local` rewrite (an IPv4 peer only), one peer per device with the newest offer winning, at most four, revoking a
//! device, refusing what is not an offer, and closing a peer that never connects.
//!
//! No ICE server on either side, and both peers bind loopback (the mDNS test binds every interface), so nothing here
//! leaves the Mac.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::*;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use eve_lite_rtc::opus::{Decoder, Encoder, APPLICATION_VOIP};
use eve_lite_rtc::{rewrite_local_candidates, CallDoor, Data, HubOptions, LinkDoor, RtcError, RtcHub, MAX_SDP_CHARS};
use eve_link::link_chat::door::LinkChatOptions;
use eve_link::link_chat::{LinkChat, MESSAGES_PATH, TURN_PATH};
use eve_phone_call::PhoneCalls;

use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_OPUS;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};

fn loopback() -> Vec<String> {
    vec!["127.0.0.1:0".to_string()]
}

fn opus() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

// ---- the page --------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Seen {
    gathered: AtomicBool,
    state: Mutex<String>,
    link_open: AtomicBool,
    link_got: Mutex<Vec<Data>>,
    call_open: AtomicBool,
    call_got: Mutex<Vec<Value>>,
    /// the loudest sample of each frame of eve's voice that reached the page
    heard: Mutex<Vec<f64>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Seen {
    fn heard_max(&self) -> f64 {
        self.heard.lock().iter().cloned().fold(0.0, f64::max)
    }
    fn call_got(&self) -> Vec<Value> {
        self.call_got.lock().clone()
    }
    fn link_got(&self) -> Vec<Data> {
        self.link_got.lock().clone()
    }
}

struct PageHandler(Arc<Seen>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PageHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.0.gathered.store(true, Ordering::SeqCst);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        *self.0.state.lock() = state.to_string();
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let seen = self.0.clone();
        let task = tokio::spawn(async move {
            let mut decoder = Decoder::new(48000, 2).unwrap();
            while let Some(event) = track.poll().await {
                match event {
                    TrackRemoteEvent::OnRtpPacket(p) => {
                        if let Ok(pcm) = decoder.decode(&p.payload) {
                            let max = pcm.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
                            seen.heard.lock().push(max as f64);
                        }
                    }
                    TrackRemoteEvent::OnEnded => return,
                    _ => {}
                }
            }
        });
        self.0.tasks.lock().push(task);
    }
}

/// The web app, as a WebRTC peer: the `link` channel made before the offer, one sendrecv audio transceiver whose
/// track sends nothing until voice mode puts the microphone on it (`replace_track` in the page; here the tone starts
/// on the same track, no renegotiation), and the `call` channel opened on demand.
struct Page {
    pc: Arc<dyn PeerConnection>,
    seen: Arc<Seen>,
    link: Arc<dyn DataChannel>,
    call: Mutex<Option<Arc<dyn DataChannel>>>,
    mic: Arc<TrackLocalStaticSample>,
    mic_ssrc: u32,
    toning: Arc<AtomicBool>,
}

fn watch(dc: Arc<dyn DataChannel>, seen: Arc<Seen>, call: bool) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = dc.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    if call { &seen.call_open } else { &seen.link_open }.store(true, Ordering::SeqCst);
                }
                DataChannelEvent::OnMessage(m) => {
                    if call {
                        if let Ok(v) = serde_json::from_slice::<Value>(&m.data) {
                            seen.call_got.lock().push(v);
                        }
                    } else if m.is_string {
                        seen.link_got.lock().push(Data::Text(String::from_utf8_lossy(&m.data).to_string()));
                    } else {
                        seen.link_got.lock().push(Data::Binary(m.data.to_vec()));
                    }
                }
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
    })
}

impl Page {
    async fn new(udp_addrs: Vec<String>) -> Page {
        let seen = Arc::new(Seen::default());
        let mut media = MediaEngine::default();
        media.register_codec(RTCRtpCodecParameters { rtp_codec: opus(), payload_type: 111 }, RtpCodecKind::Audio).unwrap();
        let pc = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(vec![]).build())
            .with_media_engine(media)
            .with_handler(Arc::new(PageHandler(seen.clone())))
            .with_udp_addrs(udp_addrs)
            .build()
            .await
            .unwrap();
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        let link: Arc<dyn DataChannel> = pc.create_data_channel("link", None).await.unwrap();
        seen.tasks.lock().push(watch(link.clone(), seen.clone(), false));
        let mic_ssrc = rand::random::<u32>();
        let mic = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    "page-mic".to_owned(),
                    "page-mic-track".to_owned(),
                    "the page's microphone".to_owned(),
                    RtpCodecKind::Audio,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters { ssrc: Some(mic_ssrc), ..Default::default() },
                        codec: opus(),
                        ..Default::default()
                    }],
                ),
            )
            .unwrap(),
        );
        pc.add_transceiver_from_track(
            mic.clone() as Arc<dyn TrackLocal>,
            Some(RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Sendrecv, ..Default::default() }),
        )
        .await
        .unwrap();
        Page { pc, seen, link, call: Mutex::new(None), mic, mic_ssrc, toning: Arc::new(AtomicBool::new(false)) }
    }

    async fn loopback() -> Page {
        Page::new(loopback()).await
    }

    async fn offer(&self) -> String {
        let offer = self.pc.create_offer(None).await.unwrap();
        self.pc.set_local_description(offer).await.unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !self.seen.gathered.load(Ordering::SeqCst) {
            assert!(Instant::now() < end, "the page gathered no candidates");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.pc.local_description().await.unwrap().sdp
    }

    async fn accept(&self, sdp: &str) {
        self.pc.set_remote_description(RTCSessionDescription::answer(sdp.to_string()).unwrap()).await.unwrap();
    }

    async fn send_link(&self, msg: &Value) {
        self.link.send_text(&eve_lite_rtc::py::dumps(msg, false)).await.unwrap();
    }

    fn link_answer(&self, rid: u32) -> Option<Answer> {
        answers(&self.seen.link_got()).get(&rid).cloned()
    }

    fn link_ended(&self, rid: u32) -> bool {
        self.link_answer(rid).is_some_and(|a| a.ended())
    }

    async fn open_call(&self) {
        let call: Arc<dyn DataChannel> = self.pc.create_data_channel("call", None).await.unwrap();
        self.seen.tasks.lock().push(watch(call.clone(), self.seen.clone(), true));
        *self.call.lock() = Some(call);
    }

    async fn say(&self, msg: Value) {
        let call = self.call.lock().clone().expect("a call channel");
        call.send_text(&eve_lite_rtc::py::dumps(&msg, false)).await.unwrap();
    }

    /// The microphone goes on: a 1 kHz sine, 48 kHz stereo as a browser encodes it, 20 ms packets in real time.
    fn tone(&self, hz: f64) {
        self.toning.store(true, Ordering::SeqCst);
        let (mic, ssrc, on) = (self.mic.clone(), self.mic_ssrc, self.toning.clone());
        let task = tokio::spawn(async move {
            let mut enc = Encoder::new(48000, 2, APPLICATION_VOIP).unwrap();
            let start = Instant::now();
            let mut n: usize = 0;
            while on.load(Ordering::SeqCst) {
                let due = start + Duration::from_secs_f64(n as f64 / 48000.0);
                tokio::time::sleep_until(due.into()).await;
                let mut pcm = Vec::with_capacity(1920);
                for i in 0..960 {
                    let v = ((2.0 * std::f64::consts::PI * hz * (n + i) as f64 / 48000.0).sin() * 8000.0) as i16;
                    pcm.push(v);
                    pcm.push(v);
                }
                n += 960;
                let packet = enc.encode(&pcm).unwrap();
                let sample = Sample { data: Bytes::from(packet), duration: Duration::from_millis(20), ..Sample::new(Instant::now()) };
                let _ = mic.write_sample(ssrc, 111, &sample, &[]).await;
            }
        });
        self.seen.tasks.lock().push(task);
    }

    fn link_open(&self) -> bool {
        self.seen.link_open.load(Ordering::SeqCst)
    }

    async fn close(&self) {
        self.toning.store(false, Ordering::SeqCst);
        let _ = self.pc.close().await;
        for t in self.seen.tasks.lock().drain(..) {
            t.abort();
        }
    }
}

/// The offer with only its first host candidate. A peer that will never connect then has one pair to check.
fn one_candidate(sdp: &str) -> String {
    let first = sdp
        .split("\r\n")
        .find(|ln| ln.starts_with("a=candidate:") && ln.contains(" typ host"))
        .expect("a host candidate")
        .to_string();
    sdp.split("\r\n").filter(|ln| !ln.starts_with("a=candidate:") || *ln == first).collect::<Vec<_>>().join("\r\n")
}

fn opts(udp: Vec<String>) -> HubOptions {
    HubOptions { udp_addrs: udp, ..HubOptions::default() }
}

fn hub_with(link: Option<&LinkChat>, calls: Option<Arc<PhoneCalls>>, o: HubOptions) -> RtcHub {
    let link: Option<Arc<dyn LinkDoor>> = link.map(|l| Arc::new(l.clone()) as Arc<dyn LinkDoor>);
    let calls: Option<Arc<dyn CallDoor>> = calls.map(|c| c as Arc<dyn CallDoor>);
    RtcHub::with_options(link, calls, o)
}

async fn wait(pred: impl Fn() -> bool, secs: f64) {
    until(pred, secs).await
}

// ---- the whole path ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_peers_complete_a_turn_over_link_and_a_held_tone_reaches_the_call() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), Some(two_lines()));
    let link = door(tmp.path(), inlet.clone(), fast());
    let brain = FakeBrain::new("It is three o'clock.");
    let voice = FakeVoice::tone();
    let calls = Arc::new(PhoneCalls::new(Arc::new(|_: &str| None), Some(brain.clone()), Some(voice.clone())));
    let hub = hub_with(Some(&link), Some(calls.clone()), opts(loopback()));
    let page = Page::loopback().await;
    let run = async {
        let got = hub.answer("phone-1", Some(&page.offer().await), "127.0.0.1").await.unwrap();
        assert!(got.voice);
        assert!(got.sdp.contains("a=candidate:") && !got.sdp.contains("typ srflx")); // every candidate, no STUN
        assert!(got.sdp.contains("a=sendrecv")); // eve's voice on the page's audio
        page.accept(&got.sdp).await;
        wait(|| page.link_open(), 20.0).await;

        // a turn over link
        page.send_link(&req(1.into(), "POST", TURN_PATH, Some(json!({"text": "hello", "client_id": "loop-turn-1"})), "", &[]))
            .await;
        wait(|| page.link_ended(1), 10.0).await;
        let a = page.link_answer(1).unwrap();
        assert!(a.status() == 200 && a.end.as_deref() == Some("end"));
        let (contents, done, _) = events(&a.body);
        assert!(contents == ["First line.", "\n\nSecond line."] && done);
        assert_eq!(inlet.heard(), vec![("hello".to_string(), false, Some("loop-turn-1".to_string()))]);
        let stats = hub.stats();
        assert!(stats["link_turns"] == 1 && stats["connected"] == 1 && stats["peers"] == 1, "{stats:?}");

        // voice: the microphone goes on, the call channel opens, then a one-second press of a 1 kHz tone
        page.tone(1000.0);
        page.open_call().await;
        wait(|| page.seen.call_got().contains(&json!({"type": "state", "state": "connected"})), 10.0).await;
        wait(|| page.seen.call_open.load(Ordering::SeqCst), 10.0).await;
        tokio::time::sleep(Duration::from_millis(500)).await; // the tone flows before the press
        page.say(json!({"type": "talk"})).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        page.say(json!({"type": "done"})).await;
        wait(|| voice.pcm().is_some(), 10.0).await;
        let press = voice.pcm().unwrap();
        let seconds = press.len() as f64 / eve_phone_call::BYTES_PER_SEC as f64;
        assert!((1.0..=2.5).contains(&seconds), "{seconds}"); // the press, its pre- and post-roll
        let hz = dominant_hz(&press, eve_lite_rtc::SAMPLE_RATE);
        assert!((hz - 1000.0).abs() <= 15.0, "{hz}"); // the tone, through Opus and back
        assert!(i16s(&press).iter().map(|s| (*s as i32).abs()).max().unwrap() > 4000);

        // the answer comes back as sound on the page's track, its caption as its sound starts
        wait(|| page.seen.call_got().iter().any(|m| m["type"] == "said"), 10.0).await;
        wait(|| page.seen.heard_max() > 2000.0, 10.0).await;
        let kinds: Vec<String> = page.seen.call_got().iter().map(|m| m["type"].as_str().unwrap_or("").to_string()).collect();
        let at = |k: &str| kinds.iter().position(|x| x == k).unwrap();
        assert!(at("heard") < at("said"));
        assert!(page.seen.call_got().contains(&json!({"type": "said", "text": "It is three o'clock."})));
        assert!(brain.heard() == ["what time is it"] && hub.stats()["calls"] == 1);
        page.say(json!({"type": "end"})).await;
        wait(|| page.seen.call_got().contains(&json!({"type": "ended", "reason": eve_phone_call::HUNG_UP})), 10.0).await;
        wait(|| calls.active().is_none(), 5.0).await;
    };
    tokio::time::timeout(Duration::from_secs(60), run).await.expect("the session took over a minute");
    page.close().await;
    hub.close().await;
    link.close();
    assert!(hub.peer_ids().is_empty());
}

// ---- ICE, candidates and peers --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_the_peers_have_no_ice_server_at_all() {
    assert!(eve_lite_rtc::configuration().ice_servers().is_empty());
    let hub = hub_with(None, None, opts(loopback()));
    let page = Page::loopback().await;
    let got = hub.answer("laptop-1", Some(&one_candidate(&page.offer().await)), "192.168.1.20").await.unwrap();
    let ice = hub.peer("laptop-1").unwrap().pc().unwrap().get_configuration().await;
    assert!(ice.ice_servers().is_empty()); // no STUN server, no TURN server
    assert!(!got.sdp.contains("typ srflx") && !got.sdp.contains("typ relay"));
    assert!(!got.voice); // no calls on this hub
    tokio::time::sleep(Duration::from_millis(100)).await;
    page.close().await;
    hub.close().await;
}

const CANDIDATES: &str = "a=candidate:1 1 udp 2122260223 0a1b2c3d-1111-2222-3333-444455556666.local 54321 typ host generation 0 network-id 1 network-cost 10\r\n\
a=candidate:2 1 udp 2122194687 F2E1D0C9-AAAA-BBBB-CCCC-DDDDEEEEFFFF.Local 50000 typ host generation 0\r\n\
a=candidate:3 1 udp 1686052607 0a1b2c3d-1111-2222-3333-444455556666.local 61000 typ srflx raddr 0.0.0.0 rport 0\r\n\
a=candidate:4 1 udp 2122129151 192.168.1.50 54400 typ host\r\n";

fn sdp() -> String {
    format!("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n{CANDIDATES}")
}

#[test]
fn test_mdns_host_candidates_point_at_an_ipv4_peer_only() {
    let cases: [(&str, Option<&str>); 14] = [
        ("192.168.1.23", Some("192.168.1.23")),
        ("10.0.0.7", Some("10.0.0.7")),
        ("::ffff:192.168.1.23", Some("192.168.1.23")),
        ("[::ffff:192.168.1.23]", Some("192.168.1.23")),
        ("169.254.3.4", Some("169.254.3.4")),
        ("127.0.0.1", None),
        ("::1", None),
        ("fe80::1%en0", None),
        ("2001:db8::5", None),
        ("fd00::5", None),
        ("0.0.0.0", None),
        ("224.0.0.251", None),
        ("not an address", None),
        ("", None),
    ];
    let sdp = sdp();
    for (peer, expect) in cases {
        let (out, n) = rewrite_local_candidates(&sdp, peer);
        let Some(expect) = expect else {
            assert_eq!((out.as_str(), n), (sdp.as_str(), 0), "{peer}");
            continue;
        };
        assert_eq!(n, 2, "{peer}");
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert!(lines.contains(&format!("a=candidate:1 1 udp 2122260223 {expect} 54321 typ host generation 0 network-id 1 network-cost 10").as_str()));
        assert!(lines.contains(&format!("a=candidate:2 1 udp 2122194687 {expect} 50000 typ host generation 0").as_str()));
        assert!(out.contains("0a1b2c3d-1111-2222-3333-444455556666.local 61000 typ srflx")); // only host candidates
        assert!(lines.contains(&"a=candidate:4 1 udp 2122129151 192.168.1.50 54400 typ host")); // an address stays
        assert!(out.matches("\r\n").count() == sdp.matches("\r\n").count() && out.replace(expect, "") != out);
        let (lf, m) = rewrite_local_candidates(&sdp.replace("\r\n", "\n"), peer); // line endings are kept
        assert!(m == 2 && !lf.contains('\r'));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_an_offer_hidden_behind_mdns_names_connects_through_the_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    let link = door(tmp.path(), FakeInlet::new(Some(store_at(tmp.path())), None), fast());
    let every = vec!["0.0.0.0:0".to_string()];
    let hub = hub_with(Some(&link), None, opts(every.clone()));
    let page = Page::new(every).await;
    let offer = page.offer().await;
    let host = regex::Regex::new(r"^(a=candidate:\S+ \d+ udp \d+ )(\S+)( \d+ typ host.*)$").unwrap();
    let v4: Vec<String> = offer
        .split("\r\n")
        .filter_map(|ln| host.captures(ln))
        .map(|c| c[2].to_string())
        .filter(|a| !a.contains(':') && !a.starts_with("127."))
        // a phone's address, as the signaling request carries it: never a .0 or .255 one (a VM bridge on this Mac
        // holds 192.168.97.0, and macOS will not deliver a datagram sent to it)
        .filter(|a| !a.ends_with(".0") && !a.ends_with(".255"))
        .collect();
    if v4.is_empty() {
        eprintln!("skipped: this Mac has no IPv4 address for the peers to meet on");
        page.close().await;
        hub.close().await;
        link.close();
        return;
    }
    // as a browser sends it: every IPv4 host candidate behind an mDNS name, and no IPv6 one to fall back on
    let mut lines: Vec<String> = Vec::new();
    for line in offer.split("\r\n") {
        let m = host.captures(line);
        if let Some(c) = &m {
            if c[2].contains(':') || c[2].starts_with("127.") {
                continue;
            }
        }
        let line = if line.starts_with("c=IN IP4 ") { "c=IN IP4 0.0.0.0".to_string() } else { line.to_string() };
        lines.push(match m {
            Some(c) => format!("{}{}.local{}", &c[1], uuid::Uuid::new_v4(), &c[3]),
            None => line,
        });
    }
    let hidden = lines.join("\r\n");
    let candidates: Vec<&str> = hidden.split("\r\n").filter(|ln| ln.starts_with("a=candidate:")).collect();
    assert!(!candidates.is_empty() && candidates.iter().all(|ln| ln.contains(".local ")));
    let got = hub.answer("phone-2", Some(&hidden), &v4[0]).await.unwrap();
    let remote = hub.peer("phone-2").unwrap().pc().unwrap().remote_description().await.unwrap().sdp;
    let seen: Vec<&str> = remote.split("\r\n").filter(|ln| ln.starts_with("a=candidate:")).collect();
    assert!(!remote.contains(".local") && seen.len() == candidates.len());
    assert!(seen.iter().all(|ln| ln.contains(&format!(" {} ", v4[0]))));
    page.accept(&got.sdp).await;
    wait(|| page.link_open(), 20.0).await; // it connected that way
    page.send_link(&get(1, MESSAGES_PATH)).await;
    wait(|| page.link_ended(1), 10.0).await;
    assert_eq!(page.link_answer(1).unwrap().status(), 200);
    page.close().await;
    hub.close().await;
    link.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_one_peer_per_device_the_newest_offer_wins_and_at_most_four() {
    let hub = hub_with(None, None, opts(loopback()));
    let mut pages = Vec::new();
    for _ in 0..6 {
        pages.push(Page::loopback().await);
    }
    for (i, page) in pages.iter().enumerate().take(5) {
        hub.answer(&format!("device-{i}"), Some(&one_candidate(&page.offer().await)), "192.168.1.30").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(hub.peer_ids(), (1..5).map(|i| format!("device-{i}")).collect::<Vec<_>>()); // the oldest made room
    let old = hub.peer("device-2").unwrap();
    hub.answer("device-2", Some(&one_candidate(&pages[5].offer().await)), "192.168.1.30").await.unwrap(); // same device
    assert!(!Arc::ptr_eq(&hub.peer("device-2").unwrap(), &old) && old.closed());
    assert_eq!(hub.peer_ids(), vec!["device-1", "device-3", "device-4", "device-2"]);
    wait(|| old.pc().is_none(), 5.0).await;
    assert!(hub.stats()["answers"] == 6 && hub.stats()["peers"] == 4);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let peers: Vec<_> = hub.peer_ids().iter().filter_map(|d| hub.peer(d)).collect();
    let late = Page::loopback().await;
    let offer = late.offer().await;
    pages.push(late);
    hub.close().await;
    assert!(hub.peer_ids().is_empty() && peers.iter().all(|p| p.closed() && p.pc().is_none()));
    assert!(hub.answer("device-9", Some(&offer), "192.168.1.30").await.is_err()); // a stopping hub takes none
    for page in &pages {
        page.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_what_is_not_an_offer_with_a_data_channel_is_refused() {
    let long = "x".repeat(MAX_SDP_CHARS + 1);
    let cases: [Option<&str>; 7] = [
        Some(""),
        Some("   "),
        Some(&long),
        None, // not a string at all (None, 5)
        None,
        Some("v=0\r\nnot really sdp\r\n"),
        Some("v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n"),
    ];
    for sdp in cases {
        let hub = hub_with(None, None, opts(loopback()));
        let refused: RtcError = hub.answer("device-1", sdp, "192.168.1.2").await.unwrap_err();
        if sdp.is_some_and(|s| s.starts_with("v=0")) {
            assert_eq!(refused.to_string(), "the offer has no data channel");
        }
        assert!(hub.peer_ids().is_empty() && hub.stats()["answers"] == 0);
        hub.close().await;
    }
}

#[tokio::test]
async fn test_without_aiortc_every_offer_is_refused_and_nothing_else_breaks() {
    // as if the WebRTC stack's codec did not load
    let hub = hub_with(None, None, HubOptions { available: Some(Arc::new(|| false)), ..opts(loopback()) });
    let refused = hub.answer("device-1", Some(&sdp()), "192.168.1.2").await.unwrap_err();
    assert!(refused.to_string().contains("not available"));
    assert!(hub.peer_ids().is_empty() && hub.stats()["answers"] == 0);
    hub.close().await;
    assert!(eve_lite_rtc::available()); // positive control: this Mac has libopus
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_device_id_is_needed_and_a_peer_that_never_connects_is_closed() {
    let hub = hub_with(None, None, HubOptions { connect_secs: 0.5, ..opts(loopback()) });
    let page = Page::loopback().await;
    let offer = one_candidate(&page.offer().await);
    assert!(hub.answer("", Some(&offer), "192.168.1.2").await.is_err());
    hub.answer("device-1", Some(&offer), "192.168.1.2").await.unwrap(); // the page never applies it
    let peer = hub.peer("device-1").unwrap();
    wait(|| hub.peer_ids().is_empty() && peer.closed() && peer.pc().is_none(), 5.0).await;
    page.close().await;
    hub.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_revoking_a_device_ends_its_requests_and_its_call() {
    let tmp = tempfile::tempdir().unwrap();
    let inlet = FakeInlet::new(Some(store_at(tmp.path())), None);
    let link = door(tmp.path(), inlet, LinkChatOptions { poll_secs: 0.05, ..fast() });
    let calls = Arc::new(PhoneCalls::new(Arc::new(|_: &str| None), Some(FakeBrain::default_reply()), Some(FakeVoice::calendar())));
    let hub = hub_with(Some(&link), Some(calls.clone()), opts(loopback()));
    let page = Page::loopback().await;
    let run = async {
        let got = hub.answer("phone-3", Some(&page.offer().await), "127.0.0.1").await.unwrap();
        page.accept(&got.sdp).await;
        wait(|| page.link_open(), 20.0).await;
        page.send_link(&get(1, MESSAGES_PATH)).await;
        wait(|| page.link_ended(1), 10.0).await;
        let cursor = match &page.link_answer(1).unwrap().json()["cursor"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        page.send_link(&req(2.into(), "GET", MESSAGES_PATH, None, &format!("after={cursor}&wait=25"), &[])).await; // a long poll
        page.open_call().await;
        wait(|| calls.active().is_some(), 10.0).await;
        let peer = hub.peer("phone-3").unwrap();
        wait(|| peer.link().is_some_and(|l| l.requests().contains(&2)), 10.0).await;
        let poll = peer.link().unwrap().request(2).unwrap();
        hub.close_device("phone-3").await;
        assert!(hub.peer_ids().is_empty() && peer.closed());
        wait(|| calls.active().is_none() && poll.task_done(), 10.0).await;
        assert!(page.link_answer(2).is_none()); // never answered: cancelled
        hub.close_device("phone-3").await; // twice: nothing to close
    };
    tokio::time::timeout(Duration::from_secs(40), run).await.expect("the revoke took over 40 s");
    page.close().await;
    hub.close().await;
    link.close();
}

// ---- the channels a peer takes ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_a_peer_takes_one_link_and_one_call_channel_and_refuses_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let link = door(tmp.path(), FakeInlet::new(Some(store_at(tmp.path())), None), fast());
    let calls = Arc::new(PhoneCalls::new(Arc::new(|_: &str| None), Some(FakeBrain::default_reply()), Some(FakeVoice::calendar())));
    let hub = hub_with(Some(&link), Some(calls.clone()), opts(loopback()));
    let peer = eve_lite_rtc::Peer::new(&hub, "device-1");
    for refused in [FakeChannel::new("video"), FakeChannel::with("link", false), FakeChannel::new("")] {
        peer.on_channel(refused.clone());
        assert!(refused.is_closed() && peer.link().is_none());
    }
    let lossy = FakeChannel::new("call");
    *lossy.max_retransmits.lock() = Some(0);
    peer.on_channel(lossy.clone());
    assert!(lossy.is_closed() && peer.call().is_none());
    let (first, second) = (FakeChannel::new("link"), FakeChannel::new("link"));
    peer.on_channel(first.clone());
    peer.on_channel(second.clone()); // the newest wins
    let now: Arc<dyn eve_lite_rtc::Channel> = second.clone();
    assert!(peer.link().is_some_and(|l| Arc::ptr_eq(l.channel(), &now)) && first.is_closed());
    // a connection whose offer had no audio cannot take a call: the page is told why
    let no_audio = FakeChannel::new("call");
    peer.on_channel(no_audio.clone());
    assert!(no_audio.is_closed() && peer.call().is_none());
    assert_eq!(no_audio.got(), vec![json!({"type": "ended", "reason": eve_lite_rtc::NO_AUDIO})]);
    // with voice on, the call channel goes to PhoneCalls with the peer's secret as its hello's token
    peer.set_voice_on(true);
    let call = FakeChannel::new("call");
    peer.on_channel(call.clone());
    wait(|| call.got().contains(&json!({"type": "state", "state": "connected"})), 5.0).await;
    assert!(calls.active().is_some() && peer.call().is_some());
    let call2 = FakeChannel::new("call");
    peer.on_channel(call2.clone()); // a second call on this peer
    assert!(call.is_closed());
    wait(|| call2.got().contains(&json!({"type": "state", "state": "connected"})), 5.0).await;
    peer.close().await;
    assert!(call2.is_closed() && second.is_closed());
    wait(|| calls.active().is_none(), 5.0).await;
    link.close();
}
