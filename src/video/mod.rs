//! The picture: capture the desktop, encode HEVC, hand each access unit to the hub in the layout
//! VideoToolbox gives the Mac host (HEVCEncoder.swift): parameter sets (VPS, SPS, PPS, raw NALs)
//! with each keyframe, and frames as 4-byte length-prefixed NALs without them.

use crate::server::Hub;
use std::sync::{Arc, Mutex};

#[cfg(windows)]
mod windows;

#[cfg_attr(not(windows), allow(dead_code))]
pub struct Settings {
    pub fps: u32,
    /// The picture's longer side is scaled down to at most this many pixels.
    pub max_side: u32,
}

/// One Annex B access unit (start codes, as Media Foundation encoders write it) in the Mac's layout.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq)]
pub struct AccessUnit {
    /// Length-prefixed NALs (4 bytes, big endian), parameter sets and delimiters left out.
    pub frame: Vec<u8>,
    /// VPS, SPS and PPS in that order, when the unit carried all three.
    pub parameter_sets: Option<Vec<Vec<u8>>>,
    /// An IDR, CRA or BLA picture: a frame a decoder can start from.
    pub is_keyframe: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Splits Annex B on 3- or 4-byte start codes.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let mut end = if n + 1 < starts.len() { starts[n + 1] - 3 } else { data.len() };
        while end > s && data[end - 1] == 0 {
            end -= 1; // the leading zero of a 4-byte start code, or trailing zero bytes
        }
        if end > s {
            out.push(&data[s..end]);
        }
    }
    out
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn access_unit(annex_b: &[u8]) -> AccessUnit {
    let (mut vps, mut sps, mut pps) = (None, None, None);
    let mut frame = Vec::with_capacity(annex_b.len());
    let mut is_keyframe = false;
    for nal in nal_units(annex_b) {
        match (nal[0] >> 1) & 0x3f {
            32 => vps = Some(nal.to_vec()),
            33 => sps = Some(nal.to_vec()),
            34 => pps = Some(nal.to_vec()),
            35 => {} // access unit delimiter
            t => {
                if (16..=21).contains(&t) {
                    is_keyframe = true;
                }
                frame.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                frame.extend_from_slice(nal);
            }
        }
    }
    let parameter_sets = match (vps, sps, pps) {
        (Some(v), Some(s), Some(p)) => Some(vec![v, s, p]),
        _ => None,
    };
    AccessUnit { frame, parameter_sets, is_keyframe }
}

/// Starts the capture and encoder thread. `rect` is kept up to date with the captured monitor's
/// place on the virtual desktop, for the input mapping.
pub fn start(hub: Arc<Hub>, settings: Settings, rect: Arc<Mutex<crate::input::Rect>>) {
    #[cfg(windows)]
    windows::start(hub, settings, rect);
    #[cfg(not(windows))]
    synthetic(hub, settings, rect);
}

/// Outside Windows (development and protocol tests): no capture, a stand-in stream of the Swift
/// fixtures' parameter sets and one tiny keyframe a second, so a client sees the right sequence.
#[cfg(not(windows))]
fn synthetic(hub: Arc<Hub>, settings: Settings, _rect: Arc<Mutex<crate::input::Rect>>) {
    let _ = settings;
    std::thread::spawn(move || {
        let ps = crate::protocol::encode_parameter_sets(4, &[
            hex("40010c01ffff016000000300b0000003000003005dac09"),
            hex("420101016000000300b0000003000003005da00280802d16"),
            hex("4401c172b46240"),
        ]);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if hub.client_count() > 0 && hub.take_keyframe_request() {
                hub.broadcast_frame(&[0, 0, 0, 3, 0x26, 0x01, 0xaf], true, Some(ps.clone()));
            }
        }
    });
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_to_length_prefixed() {
        // AUD, VPS, SPS, PPS, SEI, IDR (4-byte start codes), then a trailing-zero-padded end.
        let au = [
            &[0, 0, 0, 1, 0x46, 0x01, 0x50][..],
            &[0, 0, 0, 1, 0x40, 0x01, 0x0c][..],
            &[0, 0, 1, 0x42, 0x01, 0x01][..],
            &[0, 0, 1, 0x44, 0x01, 0xc1][..],
            &[0, 0, 1, 0x4e, 0x01, 0x05, 0x80][..],
            &[0, 0, 0, 1, 0x26, 0x01, 0xaf, 0x00, 0x00, 0x03, 0x01, 0x00][..],
        ]
        .concat();
        let a = access_unit(&au);
        assert!(a.is_keyframe);
        assert_eq!(a.parameter_sets, Some(vec![vec![0x40, 0x01, 0x0c], vec![0x42, 0x01, 0x01], vec![0x44, 0x01, 0xc1]]));
        assert_eq!(a.frame, [&[0, 0, 0, 4, 0x4e, 0x01, 0x05, 0x80][..], &[0, 0, 0, 7, 0x26, 0x01, 0xaf, 0x00, 0x00, 0x03, 0x01][..]].concat());
        // A delta (TRAIL_R, type 1) is no keyframe and carries no parameter sets.
        let d = access_unit(&[0, 0, 0, 1, 0x02, 0x01, 0xd0, 0x09]);
        assert!(!d.is_keyframe);
        assert_eq!(d.parameter_sets, None);
        assert_eq!(d.frame, vec![0, 0, 0, 4, 0x02, 0x01, 0xd0, 0x09]);
    }
}
