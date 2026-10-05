use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::model::{Participant, RoomId, RtcConfig};

pub type SignalSink = Arc<dyn Fn(RoomId, i64, Value) + Send + Sync>;

pub type ActivitySink = Arc<dyn Fn(bool, i32) + Send + Sync>;

#[derive(Clone, PartialEq, Debug)]
pub enum PeerLink {
    Connecting,
    Connected,
    Failed(String),
    Closed,
}

#[derive(Clone, Default, Debug)]
pub struct PeerStatus {
    pub label: String,
    pub screen: bool,
    pub muted: bool,
    pub deafened: bool,
    pub speaking: bool,
}

#[derive(Clone, Default)]
pub struct MediaSnapshot {
    pub mic_working: bool,
    pub mic_level_db: i32,
    pub capture_error: Option<String>,
    pub screen_active: bool,
    pub peers: HashMap<i64, PeerStatus>,
    pub links: HashMap<i64, PeerLink>,
}

#[derive(Default)]
struct Shared {
    peers: Mutex<HashMap<i64, PeerStatus>>,
    links: Mutex<HashMap<i64, PeerLink>>,
    speaking: Mutex<HashMap<i64, bool>>,
    mic_level_db: AtomicI64,
    mic_working: AtomicBool,
    screen_active: AtomicBool,
    capture_error: Mutex<Option<String>>,
}

#[derive(Clone, Default)]
pub struct Media {
    shared: Arc<Shared>,
}

impl Media {
    pub fn new() -> Self {
        Media::default()
    }

    pub fn snapshot(&self) -> MediaSnapshot {
        let speaking = self.shared.speaking.lock().unwrap().clone();
        let mut peers = self.shared.peers.lock().unwrap().clone();
        for (id, active) in &speaking {
            if let Some(peer) = peers.get_mut(id) {
                peer.speaking = *active;
            }
        }
        MediaSnapshot {
            mic_working: self.shared.mic_working.load(Ordering::Relaxed),
            mic_level_db: self.shared.mic_level_db.load(Ordering::Relaxed) as i32,
            capture_error: self.shared.capture_error.lock().unwrap().clone(),
            screen_active: self.shared.screen_active.load(Ordering::Relaxed),
            peers,
            links: self.shared.links.lock().unwrap().clone(),
        }
    }

    pub fn set_link(&self, user_id: i64, link: PeerLink) {
        self.shared.links.lock().unwrap().insert(user_id, link);
    }

    pub fn update_peer(&self, user_id: i64, participant: &Participant, label: String) {
        let mut peers = self.shared.peers.lock().unwrap();
        let entry = peers.entry(user_id).or_default();
        entry.label = label;
        entry.screen = participant.screen;
        entry.muted = participant.muted;
        entry.deafened = participant.deafened;
    }

    pub fn forget(&self, user_id: i64) {
        self.shared.links.lock().unwrap().remove(&user_id);
        self.shared.peers.lock().unwrap().remove(&user_id);
        self.shared.speaking.lock().unwrap().remove(&user_id);
    }

    pub fn clear(&self) {
        self.shared.peers.lock().unwrap().clear();
        self.shared.links.lock().unwrap().clear();
        self.shared.speaking.lock().unwrap().clear();
        self.shared.screen_active.store(false, Ordering::Relaxed);
    }

    fn set_mic_level(&self, db: i64) {
        self.shared.mic_level_db.store(db, Ordering::Relaxed);
    }

    fn set_mic_working(&self, working: bool) {
        self.shared.mic_working.store(working, Ordering::Relaxed);
    }

    fn set_capture_error(&self, error: Option<String>) {
        *self.shared.capture_error.lock().unwrap() = error;
    }

    fn set_screen_active(&self, active: bool) {
        self.shared.screen_active.store(active, Ordering::Relaxed);
    }
}

use webrtc::api::APIBuilder;
use webrtc::api::media_engine::MediaEngine;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTPCodecType};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_remote::TrackRemote;

type PeerPc = Arc<RTCPeerConnection>;
type SampleTrack = Arc<TrackLocalStaticSample>;

async fn new_peer_connection(config: &RTCConfiguration) -> Result<PeerPc, String> {
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_default_codecs()
        .map_err(|e| format!("codecs: {e}"))?;
    let registry = webrtc::api::interceptor_registry::register_default_interceptors(
        Registry::new(),
        &mut media_engine,
    )
    .map_err(|e| format!("interceptors: {e}"))?;
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();
    api.new_peer_connection(config.clone())
        .await
        .map(Arc::new)
        .map_err(|e| format!("peer connection: {e}"))
}

struct MicSource {
    track: SampleTrack,
    stop: Arc<AtomicBool>,
}

struct ScreenSource {
    track: SampleTrack,
    child: Mutex<Option<std::process::Child>>,
    stop: Arc<AtomicBool>,
}

struct Peer {
    pc: PeerPc,

    #[allow(dead_code)]
    offerer: bool,
}

pub struct Room {
    pub id: RoomId,
    media: Media,
    peers: Mutex<HashMap<i64, Peer>>,
    me_id: i64,
    ice: Vec<webrtc::ice_transport::ice_server::RTCIceServer>,
    mic: Mutex<Option<Arc<MicSource>>>,
    screen: Mutex<Option<Arc<ScreenSource>>>,
    sink: SignalSink,
    activity: ActivitySink,
}

impl Room {
    pub async fn new(
        id: RoomId,
        me_id: i64,
        config: &RtcConfig,
        sink: SignalSink,
        activity: ActivitySink,
    ) -> Result<Arc<Self>, String> {
        let ice = build_ice_servers(config);
        let mut pc_config = RTCConfiguration {
            ice_servers: ice.clone(),
            ..Default::default()
        };
        if config.ice_transport_policy == "relay" {
            pc_config.ice_transport_policy =
                webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay;
        }

        new_peer_connection(&pc_config).await?.close().await.ok();

        Ok(Arc::new(Room {
            id,
            media: Media::new(),
            peers: Mutex::new(HashMap::new()),
            me_id,
            ice,
            mic: Mutex::new(None),
            screen: Mutex::new(None),
            sink,
            activity,
        }))
    }

    pub fn media(&self) -> Media {
        self.media.clone()
    }

    pub fn screen_running(&self) -> bool {
        self.screen
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| !s.stop.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    fn is_offerer(&self, user_id: i64) -> bool {
        self.me_id > user_id
    }

    pub async fn sync_peers(&self, roster: &[Participant]) {
        let keep: Vec<i64> = roster.iter().map(|p| p.user_id).collect();
        let stale: Vec<i64> = self
            .peers
            .lock()
            .unwrap()
            .keys()
            .copied()
            .filter(|id| *id != self.me_id && !keep.contains(id))
            .collect();
        for id in stale {
            self.drop_peer(id).await;
        }

        for participant in roster {
            if participant.user_id == self.me_id || participant.user_id == 0 {
                continue;
            }
            if self
                .peers
                .lock()
                .unwrap()
                .contains_key(&participant.user_id)
            {
                continue;
            }
            if let Err(error) = self.ensure_peer(participant.user_id).await {
                self.media
                    .set_link(participant.user_id, PeerLink::Failed(error));
            }
        }
    }

    async fn drop_peer(&self, user_id: i64) {
        let peer = self.peers.lock().unwrap().remove(&user_id);
        if let Some(peer) = peer {
            let _ = peer.pc.close().await;
        }
        self.media.forget(user_id);
    }

    async fn ensure_peer(&self, user_id: i64) -> Result<(), String> {
        let offerer = self.is_offerer(user_id);
        let config = RTCConfiguration {
            ice_servers: self.ice.clone(),
            ..Default::default()
        };
        let pc = new_peer_connection(&config).await?;

        if offerer {
            let make_init = || RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Sendrecv,
                send_encodings: vec![],
            };
            pc.add_transceiver_from_kind(RTPCodecType::Audio, Some(make_init()))
                .await
                .map_err(|e| format!("audio transceiver: {e}"))?;
            pc.add_transceiver_from_kind(RTPCodecType::Video, Some(make_init()))
                .await
                .map_err(|e| format!("video transceiver: {e}"))?;
        }

        self.wire_callbacks(&pc, user_id);

        let mic = self.mic.lock().unwrap().clone();
        if let Some(mic) = mic {
            pc.add_track(mic.track.clone())
                .await
                .map_err(|e| format!("add mic track: {e}"))?;
        }

        self.media.set_link(user_id, PeerLink::Connecting);
        self.peers.lock().unwrap().insert(
            user_id,
            Peer {
                pc: pc.clone(),
                offerer,
            },
        );

        if offerer {
            self.make_offer(user_id).await?;
        }
        Ok(())
    }

    fn wire_callbacks(&self, pc: &PeerPc, user_id: i64) {
        let sink = self.sink.clone();
        let room = self.id;
        pc.on_ice_candidate(Box::new(move |candidate| {
            let sink = sink.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    let init = match candidate.to_json() {
                        Ok(init) => init,
                        Err(_) => return,
                    };
                    let payload = json!({
                        "kind": "candidate",
                        "candidate": {
                            "candidate": init.candidate,
                            "sdpMid": init.sdp_mid,
                            "sdpMLineIndex": init.sdp_mline_index,
                        }
                    });
                    sink(room, user_id, payload);
                }
            })
        }));

        let media = self.media.clone();
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let media = media.clone();
            Box::pin(async move {
                use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
                match state {
                    RTCPeerConnectionState::Connected => {
                        media.set_link(user_id, PeerLink::Connected);
                    }
                    RTCPeerConnectionState::Failed => {
                        media.set_link(user_id, PeerLink::Failed("connection failed".into()));
                    }
                    RTCPeerConnectionState::Disconnected => {
                        media.set_link(user_id, PeerLink::Connecting);
                    }
                    RTCPeerConnectionState::Closed => {
                        media.set_link(user_id, PeerLink::Closed);
                    }
                    _ => {}
                }
            })
        }));

        let media = self.media.clone();
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let media = media.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Audio {
                    spawn_remote_audio(track);
                    let _ = media;
                } else {
                    let mut peers = media.shared.peers.lock().unwrap();
                    let entry = peers.entry(user_id).or_default();
                    entry.screen = true;
                }
            })
        }));
    }

    async fn make_offer(&self, user_id: i64) -> Result<(), String> {
        let pc = self.peer_pc(user_id)?;
        let offer = pc
            .create_offer(None)
            .await
            .map_err(|e| format!("create offer: {e}"))?;
        pc.set_local_description(offer)
            .await
            .map_err(|e| format!("set local offer: {e}"))?;

        wait_for_ice_complete(&pc).await;
        if let Some(local) = pc.local_description().await {
            let payload = json!({
                "kind": "offer",
                "sdp": { "type": local.sdp_type.to_string(), "sdp": local.sdp },
            });
            (self.sink)(self.id, user_id, payload);
        }
        Ok(())
    }

    pub async fn handle_signal(self: &Arc<Self>, from: i64, signal: &Value) {
        let kind = signal.get("kind").and_then(Value::as_str).unwrap_or("");
        let result = match kind {
            "offer" => self.on_offer(from, signal).await,
            "answer" => self.on_answer(from, signal).await,
            "candidate" => self.on_candidate(from, signal).await,
            "renegotiate" => self.restart_ice(from).await,
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.media.set_link(from, PeerLink::Failed(error));
        }
    }

    async fn on_offer(self: &Arc<Self>, from: i64, signal: &Value) -> Result<(), String> {
        if !self.is_offerer(from) {
            (self.sink)(self.id, from, json!({ "kind": "renegotiate" }));
            return Ok(());
        }
        let sdp = signal
            .get("sdp")
            .and_then(|s| s.get("sdp"))
            .and_then(Value::as_str)
            .ok_or_else(|| "offer without sdp".to_string())?
            .to_string();

        if !self.peers.lock().unwrap().contains_key(&from) {
            self.ensure_peer(from).await?;
        }
        let pc = self.peer_pc(from)?;
        pc.set_remote_description(
            RTCSessionDescription::offer(sdp).map_err(|e| format!("bad offer: {e}"))?,
        )
        .await
        .map_err(|e| format!("set remote offer: {e}"))?;
        let answer = pc
            .create_answer(None)
            .await
            .map_err(|e| format!("create answer: {e}"))?;
        pc.set_local_description(answer)
            .await
            .map_err(|e| format!("set local answer: {e}"))?;
        wait_for_ice_complete(&pc).await;
        if let Some(local) = pc.local_description().await {
            let payload = json!({
                "kind": "answer",
                "sdp": { "type": local.sdp_type.to_string(), "sdp": local.sdp },
            });
            (self.sink)(self.id, from, payload);
        }
        Ok(())
    }

    async fn on_answer(&self, from: i64, signal: &Value) -> Result<(), String> {
        let sdp = signal
            .get("sdp")
            .and_then(|s| s.get("sdp"))
            .and_then(Value::as_str)
            .ok_or_else(|| "answer without sdp".to_string())?
            .to_string();
        let pc = self.peer_pc(from)?;
        pc.set_remote_description(
            RTCSessionDescription::answer(sdp).map_err(|e| format!("bad answer: {e}"))?,
        )
        .await
        .map_err(|e| format!("set remote answer: {e}"))
    }

    async fn on_candidate(&self, from: i64, signal: &Value) -> Result<(), String> {
        let entry = signal
            .get("candidate")
            .ok_or_else(|| "candidate without payload".to_string())?;
        let raw = entry
            .get("candidate")
            .and_then(Value::as_str)
            .ok_or_else(|| "candidate without payload".to_string())?
            .to_string();

        if raw.is_empty() {
            return Ok(());
        }

        if !self.peers.lock().unwrap().contains_key(&from) {
            return Ok(());
        }
        let pc = self.peer_pc(from)?;
        pc.add_ice_candidate(webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
            candidate: raw,
            sdp_mid: entry
                .get("sdpMid")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            sdp_mline_index: entry
                .get("sdpMLineIndex")
                .and_then(Value::as_u64)
                .map(|n| n as u16),
            username_fragment: None,
        })
        .await
        .map_err(|e| format!("add candidate: {e}"))
    }

    async fn restart_ice(&self, from: i64) -> Result<(), String> {
        if !self.peers.lock().unwrap().contains_key(&from) {
            return Ok(());
        }
        let pc = self.peer_pc(from)?;
        pc.restart_ice()
            .await
            .map_err(|e| format!("restart ice: {e}"))?;
        self.make_offer(from).await
    }

    fn peer_pc(&self, user_id: i64) -> Result<PeerPc, String> {
        self.peers
            .lock()
            .unwrap()
            .get(&user_id)
            .map(|p| p.pc.clone())
            .ok_or_else(|| format!("no peer for {user_id}"))
    }

    pub async fn start_mic(self: &Arc<Self>) {
        match spawn_mic_capture(self.media.clone()) {
            Ok((track, stop)) => {
                self.media.set_capture_error(None);
                self.media.set_mic_working(true);
                let mic = Arc::new(MicSource {
                    track,
                    stop: stop.clone(),
                });
                let pcs: Vec<PeerPc> = self
                    .peers
                    .lock()
                    .unwrap()
                    .values()
                    .map(|p| p.pc.clone())
                    .collect();
                for pc in pcs {
                    let _ = pc.add_track(mic.track.clone()).await;
                }
                *self.mic.lock().unwrap() = Some(mic);

                let media = self.media.clone();
                let activity = self.activity.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut previous: Option<bool> = None;
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        let level = media.snapshot().mic_level_db;
                        let active = level > -55;
                        if previous != Some(active) {
                            previous = Some(active);
                            activity(active, level);
                        }
                    }
                });
            }
            Err(error) => {
                self.media.set_capture_error(Some(error));
                self.media.set_mic_working(false);
            }
        }
    }

    pub async fn toggle_screen(self: &Arc<Self>) -> Result<bool, String> {
        if self.screen_running() {
            self.stop_screen();
            return Ok(false);
        }
        let source = Arc::new(
            spawn_screen_capture(self.media.clone()).map_err(|e| format!("screen capture: {e}"))?,
        );
        let pcs: Vec<PeerPc> = self
            .peers
            .lock()
            .unwrap()
            .values()
            .map(|p| p.pc.clone())
            .collect();
        for pc in pcs {
            pc.add_track(source.track.clone())
                .await
                .map_err(|e| format!("add screen track: {e}"))?;
        }
        *self.screen.lock().unwrap() = Some(source);
        self.media.set_screen_active(true);
        Ok(true)
    }

    pub fn stop_screen(&self) {
        if let Some(source) = self.screen.lock().unwrap().take() {
            source.stop.store(true, Ordering::Relaxed);
            if let Some(mut child) = source.child.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.media.set_screen_active(false);
    }

    pub async fn shutdown(&self) {
        self.stop_screen();
        if let Some(mic) = self.mic.lock().unwrap().take() {
            mic.stop.store(true, Ordering::Relaxed);
        }
        self.media.set_mic_working(false);
        let peers: Vec<Peer> = self.peers.lock().unwrap().drain().map(|(_, p)| p).collect();
        for peer in peers {
            let _ = peer.pc.close().await;
        }
        self.media.clear();
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        if let Ok(mut screen) = self.screen.lock() {
            screen.take();
        }
        if let Ok(mut mic) = self.mic.lock() {
            mic.take();
        }
    }
}

fn build_ice_servers(config: &RtcConfig) -> Vec<webrtc::ice_transport::ice_server::RTCIceServer> {
    config
        .ice_servers
        .iter()
        .map(|server| webrtc::ice_transport::ice_server::RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone().unwrap_or_default(),
            credential: server.credential.clone().unwrap_or_default(),
        })
        .collect()
}

async fn wait_for_ice_complete(pc: &RTCPeerConnection) {
    for _ in 0..50 {
        if pc.ice_gathering_state()
            == webrtc::ice_transport::ice_gathering_state::RTCIceGatheringState::Complete
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn spawn_mic_capture(media: Media) -> Result<(SampleTrack, Arc<AtomicBool>), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no input device available".to_string())?;
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map_err(|e| format!("input configs: {e}"))?
        .collect();

    let chosen = ranges
        .iter()
        .find(|c| {
            c.sample_format() == cpal::SampleFormat::I16
                && c.min_sample_rate().0 <= 48_000
                && c.max_sample_rate().0 >= 48_000
                && c.channels() == 1
        })
        .or_else(|| {
            ranges
                .iter()
                .find(|c| c.sample_format() == cpal::SampleFormat::I16)
        })
        .or_else(|| ranges.first())
        .cloned()
        .ok_or_else(|| "no usable input format".to_string())?;
    let chosen = if chosen.min_sample_rate().0 <= 48_000 && chosen.max_sample_rate().0 >= 48_000 {
        chosen.with_sample_rate(cpal::SampleRate(48_000))
    } else {
        chosen.with_max_sample_rate()
    };

    let sample_format = chosen.sample_format();
    let sample_rate = chosen.sample_rate().0;
    let channels = chosen.channels().max(1);

    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: "audio/opus".to_string(),
            clock_rate: 48_000,
            channels,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
            rtcp_feedback: vec![],
        },
        "desktop".to_string(),
        "plainwire".to_string(),
    ));

    let stop = Arc::new(AtomicBool::new(false));
    let writer = track.clone();
    let writer_stop = stop.clone();
    let writer_media = media.clone();

    let frame_samples = (sample_rate as usize / 50).max(1);

    let feed = move |data: &[i16]| {
        if writer_stop.load(Ordering::Relaxed) {
            return;
        }
        let samples = to_mono(data, channels);
        writer_media.set_mic_level(rms_db(&samples));
        for chunk in samples.chunks(frame_samples) {
            let payload: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let track = writer.clone();
            let duration = std::time::Duration::from_millis(20);
            tokio::spawn(async move {
                let sample = webrtc::media::Sample {
                    data: bytes::Bytes::from(payload),
                    timestamp: std::time::SystemTime::now(),
                    duration,
                    packet_timestamp: 0,
                    prev_dropped_packets: 0,
                    prev_padding_packets: 0,
                };
                let _ = track.write_sample(&sample).await;
            });
        }
    };

    let on_error = move |_error: cpal::StreamError| {};

    let stream = match sample_format {
        cpal::SampleFormat::I16 => device.build_input_stream(
            &chosen.config(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| feed(data),
            on_error,
            None,
        ),
        cpal::SampleFormat::F32 => device.build_input_stream(
            &chosen.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let scaled: Vec<i16> = data
                    .iter()
                    .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                    .collect();
                feed(&scaled);
            },
            move |_error: cpal::StreamError| {},
            None,
        ),
        other => return Err(format!("unsupported input format: {other:?}")),
    }
    .map_err(|e| format!("build input stream: {e}"))?;

    stream.play().map_err(|e| format!("start input: {e}"))?;

    let keep = Arc::new(Mutex::new(Some(stream)));
    let keep_stop = stop.clone();
    std::thread::spawn(move || {
        while !keep_stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if let Some(stream) = keep.lock().unwrap().take() {
            let _ = stream.pause();
        }
    });

    Ok((track, stop))
}

fn to_mono(data: &[i16], channels: u16) -> Vec<i16> {
    if channels <= 1 {
        data.to_vec()
    } else {
        data.chunks(channels as usize)
            .filter_map(|frame| frame.first().copied())
            .collect()
    }
}

fn rms_db(samples: &[i16]) -> i64 {
    if samples.is_empty() {
        return -100;
    }
    let sum: f64 = samples.iter().map(|s| (*s as f64 / 32768.0).powi(2)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    (20.0 * rms.max(0.00001).log10()).clamp(-100.0, 0.0) as i64
}

fn spawn_remote_audio(track: Arc<TrackRemote>) {
    let Ok(mut decoder) = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "opus",
            "-i",
            "pipe:0",
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "1",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let Some(mut stdin) = decoder.stdin.take() else {
        return;
    };
    let Some(stdout) = decoder.stdout.take() else {
        return;
    };

    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use std::io::Read;

        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            return;
        };
        let Ok(config) = device.default_output_config() else {
            return;
        };
        let Ok(sink) = device.build_output_stream(
            &config.config(),
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut leftover: Vec<i16> = Vec::new();
                PENDING.with(|queue| {
                    for mut chunk in queue.borrow_mut().drain(..) {
                        if leftover.is_empty() {
                            let n = data.len().min(chunk.len());
                            data[..n].copy_from_slice(&chunk[..n]);
                            if n < chunk.len() {
                                leftover = chunk.split_off(n);
                            }
                        } else {
                            leftover.extend_from_slice(&chunk);
                        }
                    }
                });
                if !leftover.is_empty() {
                    PENDING.with(|queue| queue.borrow_mut().push(leftover));
                }
            },
            move |_e: cpal::StreamError| {},
            None,
        ) else {
            return;
        };
        if sink.play().is_err() {
            return;
        }

        let mut pcm = stdout;
        let mut buffer = vec![0u8; 8192];
        while let Ok(read) = pcm.read(&mut buffer) {
            if read < 2 {
                break;
            }
            let samples: Vec<i16> = buffer[..read - read % 2]
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            PENDING.with(|p| p.borrow_mut().push(samples));
        }
    });

    tokio::spawn(async move {
        let mut buffer = vec![0u8; 1500];
        while let Ok((packet, _)) = track.read(&mut buffer).await {
            if stdin.write_all(&packet.payload).is_err() {
                break;
            }
            let _ = stdin.flush();
        }
        let _ = decoder.kill();
        let _ = decoder.wait();
    });
}

thread_local! {
    static PENDING: std::cell::RefCell<Vec<Vec<i16>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn spawn_screen_capture(media: Media) -> Result<ScreenSource, String> {
    use std::process::{Command, Stdio};

    if which("ffmpeg").is_none() {
        return Err("ffmpeg is required for screen sharing but was not found".into());
    }
    let display = std::env::var("DISPLAY").unwrap_or_default();
    if display.is_empty() {
        return Err(
            "screen capture needs a DISPLAY; a Wayland-only session requires portal access".into(),
        );
    }

    let (width, height, fps) = (1280u32, 720u32, 15u32);
    let mut command = Command::new("ffmpeg");

    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "x11grab",
        "-framerate",
        &fps.to_string(),
        "-video_size",
        &format!("{width}x{height}"),
        "-i",
        &format!("{display}+0,0"),
        "-pix_fmt",
        "yuv420p",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no ffmpeg output".to_string())?;

    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: "video/VP8".to_string(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: String::new(),
            rtcp_feedback: vec![],
        },
        "screen".to_string(),
        "plainwire".to_string(),
    ));

    let stop = Arc::new(AtomicBool::new(false));
    let writer = track.clone();
    let writer_stop = stop.clone();
    let frame_size = (width * height * 3 / 2) as usize;

    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdout = stdout;
        let mut frame = vec![0u8; frame_size];
        while !writer_stop.load(Ordering::Relaxed) {
            if stdout.read_exact(&mut frame).is_err() {
                break;
            }
            let track = writer.clone();
            let payload = frame.clone();
            tokio::spawn(async move {
                let sample = webrtc::media::Sample {
                    data: bytes::Bytes::from(payload),
                    timestamp: std::time::SystemTime::now(),
                    duration: std::time::Duration::from_nanos(1_000_000_000 / fps as u64),
                    packet_timestamp: 0,
                    prev_dropped_packets: 0,
                    prev_padding_packets: 0,
                };
                let _ = track.write_sample(&sample).await;
            });
        }
    });

    media.set_capture_error(None);
    Ok(ScreenSource {
        track,
        child: Mutex::new(Some(child)),
        stop,
    })
}

fn which(binary: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}
