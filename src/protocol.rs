//! Sill's wire format, exactly as Sources/StreamProtocol (the Swift reference) writes it.
//! Header (14 bytes, big endian): kind(1) timestamp(8, f64) isKeyframe(1) payloadLength(4).
//! JSON payloads follow Swift's Codable: optional fields are left out when nil, and enums with
//! associated values are `{"case":{"_0":…,"label":…}}`. tests/conformance.rs checks every message
//! this host sends and every one it reads against bytes made by the Swift code itself.

use serde::{Deserialize, Serialize};

pub const HEADER_LEN: usize = 14;
/// Any device → host message (StreamMessage.maxClientPayload).
pub const MAX_CLIENT_PAYLOAD: usize = 1 << 20;
/// The one message of a pairing connection (maxPairingPayload).
pub const MAX_PAIRING_PAYLOAD: usize = 4096;

pub mod kind {
    pub const PARAMETER_SETS: u8 = 0;
    pub const FRAME: u8 = 1;
    pub const WINDOW_LIST: u8 = 2;
    pub const THUMBNAIL: u8 = 3;
    pub const APP_ICON: u8 = 4;
    pub const APP_LIST: u8 = 5;
    pub const SELECT_SOURCE: u8 = 6;
    pub const LAUNCH_APP: u8 = 7;
    pub const INPUT: u8 = 8;
    pub const VIEWPORT: u8 = 9;
    pub const PING: u8 = 10;
    pub const PONG: u8 = 11;
    pub const CLIENT_STATS: u8 = 12;
    pub const TICK: u8 = 13;
    pub const CURSOR_SHAPE: u8 = 14;
    pub const WINDOW_COMMAND: u8 = 15;
    pub const HOST_SETTINGS: u8 = 16;
    pub const CHANGE_SETTINGS: u8 = 17;
    pub const MAC_INFO: u8 = 18;
    pub const PAIR_REQUEST: u8 = 19;
    pub const PAIR_RESULT: u8 = 20;
    pub const GOODBYE: u8 = 22;
    pub const HELLO: u8 = 23;
    pub const MAC_POINTER: u8 = 26;
    pub const GESTURE: u8 = 28;
    pub const CLIPBOARD: u8 = 32;
    pub const AUDIO: u8 = 43;
    pub const KEYFRAME_REQUEST: u8 = 44;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub kind: u8,
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub payload_len: usize,
}

impl Header {
    pub fn parse(b: &[u8; HEADER_LEN]) -> Header {
        Header {
            kind: b[0],
            timestamp: f64::from_bits(u64::from_be_bytes(b[1..9].try_into().unwrap())),
            is_keyframe: b[9] == 1,
            payload_len: u32::from_be_bytes(b[10..14].try_into().unwrap()) as usize,
        }
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// One message, header and payload, ready to write.
pub fn message(kind: u8, timestamp: f64, is_keyframe: bool, payload: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(HEADER_LEN + payload.len());
    d.push(kind);
    d.extend_from_slice(&timestamp.to_bits().to_be_bytes());
    d.push(is_keyframe as u8);
    d.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    d.extend_from_slice(payload);
    d
}

pub fn json_message<T: Serialize>(kind: u8, value: &T) -> Vec<u8> {
    message(kind, now(), false, &serde_json::to_vec(value).expect("JSON"))
}

/// HEVC VPS/SPS/PPS: nalLen(1) count(1), then per set: length(2) bytes.
pub fn encode_parameter_sets(nal_len: u8, sets: &[Vec<u8>]) -> Vec<u8> {
    let mut d = vec![nal_len, sets.len() as u8];
    for s in sets {
        d.extend_from_slice(&(s.len() as u16).to_be_bytes());
        d.extend_from_slice(s);
    }
    d
}

// ---- Host → device -------------------------------------------------------------------------

/// windowID(4) + JPEG.
pub fn encode_thumbnail(window: u32, jpeg: &[u8]) -> Vec<u8> {
    [&window.to_be_bytes()[..], jpeg].concat()
}

/// bundleIDLength(2) + bundleID + PNG.
pub fn encode_icon(bundle_id: &str, png: &[u8]) -> Vec<u8> {
    [&(bundle_id.len() as u16).to_be_bytes()[..], bundle_id.as_bytes(), png].concat()
}

/// StreamSource: `{"none":{}}`, `{"desktop":{}}`, `{"window":{"_0":ID}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamSource {
    None {},
    Desktop {},
    Window {
        #[serde(rename = "_0")]
        id: u32,
    },
}

impl Default for StreamSource {
    fn default() -> StreamSource {
        StreamSource::Desktop {}
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowList {
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub active: StreamSource,
    #[serde(rename = "launchID", skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    /// 1: this host takes the six trackpad gestures (kind 28).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gestures: Option<i64>,
    /// 1: this host takes and sends clipboard text (kind 32).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<i64>,
    /// 1: this host repeats a held key, so a device sends a hardware keyboard's characters as keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_repeat: Option<i64>,
    /// This PC is locked: the list holds no windows and no `clipboard`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSettings {
    #[serde(rename = "maxFPS")]
    pub max_fps: i64,
    pub bitrate: i64,
    pub capture_scale: f64,
    pub prioritize_speed: bool,
    pub virtual_display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_audio: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningStream {
    pub width: i64,
    pub height: i64,
    pub fps: i64,
    pub mbps: i64,
    pub on_virtual_display: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSettingsState {
    pub settings: StreamSettings,
    pub persistent: bool,
    pub virtual_display_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_display_note: Option<String>,
    pub software_encoder: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<RunningStream>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answering: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_paired: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    #[serde(rename = "macID", skip_serializing_if = "Option::is_none")]
    pub mac_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recognition_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tries_left: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<f64>,
}

impl PairResult {
    pub fn refused(reason: &str) -> PairResult {
        PairResult { ok: false, reason: Some(reason.into()), ..Default::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goodbye {
    pub reason: String,
}

// ---- Device → host -------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerAction {
    Move,
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollPhase {
    Began,
    Ended,
    MomentumBegan,
    MomentumEnded,
}

/// InputEvent (Input.swift). Positions are fractions (0…1) of the video frame.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputEvent {
    Pointer {
        #[serde(rename = "_0")]
        action: PointerAction,
        x: f64,
        y: f64,
    },
    /// dx/dy: fractions of the frame the content should move; positive dy moves the content down.
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    Text {
        #[serde(rename = "_0")]
        text: String,
    },
    ScrollGesture {
        #[serde(rename = "_0")]
        phase: ScrollPhase,
        x: f64,
        y: f64,
    },
    /// A USB HID usage with UIKeyModifierFlags bits.
    #[serde(rename_all = "camelCase")]
    Key { hid_usage: u16, down: bool, modifiers: u64 },
}

/// Every field optional: `{}` decodes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub app_version: Option<String>,
    pub device: Option<String>,
    pub audio: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairRequest {
    pub v: i64,
    pub method: String,
    pub proof: String,
    pub name: String,
    pub model: Option<String>,
}

/// What a device reports once a second: the frames it showed, and how far behind they were.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStats {
    pub fps: i64,
    pub frame_age_ms: i64,
    pub rtt_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSettingsChange {
    pub token: Option<i64>,
    #[serde(rename = "maxFPS")]
    pub max_fps: Option<i64>,
    pub bitrate: Option<i64>,
    pub capture_scale: Option<f64>,
    pub send_audio: Option<bool>,
}

/// An installed app, for the drawer's "All apps" (kind 5 is a list of these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
}

/// Kind 7.
#[derive(Debug, Clone, Deserialize)]
pub struct LaunchApp {
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
}

/// Kind 15: "close", "minimize" or "fullScreen".
#[derive(Debug, Clone, Deserialize)]
pub struct WindowCommand {
    pub id: u32,
    pub action: String,
}

/// Kind 9: the device's panel in points and the wanted text scale.
#[derive(Debug, Clone, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    pub scale: Option<f64>,
    /// The rate this device's panel wants (nil: 60).
    pub fps: Option<i64>,
}

/// Kind 26: where the PC's pointer is while this device isn't moving it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MacPointer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    pub inside: bool,
    pub seen: u64,
}

/// Kind 28.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackpadGesture {
    pub gesture: String,
}

/// Kind 32, both ways: every field optional, left out when nil.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Clipboard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<bool>,
}

/// One way to reach this PC from afar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacAddress {
    pub host: String,
    /// "vpn", "lan" or "internet".
    pub kind: String,
    /// "Tailscale", "Wi-Fi"…: where it comes from, as the device shows it.
    pub via: String,
}

/// Inside kind 18: who this PC is and how to reach it from afar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacInfo {
    pub v: i64,
    #[serde(rename = "macID")]
    pub mac_id: String,
    pub name: String,
    pub issued_at: f64,
    pub remote_access: bool,
    pub remote_port: i64,
    pub internet: bool,
    pub addresses: Vec<MacAddress>,
}

/// Kind 18: `info` is the exact JSON bytes of a MacInfo, and `sig` covers those bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedMacInfo {
    pub info: String,
    pub key: String,
    pub sig: String,
}

/// Text from a device for the console: control characters and bidi overrides dropped, at most
/// `limit` characters (SafeText.label's job on the Mac).
pub fn safe_label(s: &str, limit: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069))
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}
