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
    pub const SELECT_SOURCE: u8 = 6;
    pub const INPUT: u8 = 8;
    pub const PING: u8 = 10;
    pub const PONG: u8 = 11;
    pub const TICK: u8 = 13;
    pub const HOST_SETTINGS: u8 = 16;
    pub const CHANGE_SETTINGS: u8 = 17;
    pub const PAIR_REQUEST: u8 = 19;
    pub const PAIR_RESULT: u8 = 20;
    pub const GOODBYE: u8 = 22;
    pub const HELLO: u8 = 23;
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct PairRequest {
    pub v: i64,
    pub method: String,
    pub proof: String,
    pub name: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostSettingsChange {
    pub token: Option<i64>,
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
