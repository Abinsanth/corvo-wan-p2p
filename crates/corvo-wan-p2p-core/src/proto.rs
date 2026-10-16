//! What two apps say to each other: frames of JSON, each sent as a 4-byte
//! big-endian length followed by the JSON bytes.

use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};

/// Bumped whenever the frames change in a way an older app can't read.
pub const PROTO_VERSION: u32 = 1;
/// The largest frame either side accepts, in bytes.
pub const MAX_FRAME: usize = 64 * 1024;
/// The longest message text, in bytes.
pub const MAX_TEXT: usize = 16 * 1024;
/// The longest message id, in bytes (a ULID is 26).
pub const MAX_ID: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Sent first, by both sides. Names follow in a [`Frame::Name`].
    /// `session` is random for each run of the app, so a link from a restarted
    /// app can be told from a stale one. Apps before 0.1.0-rc.3 send none (0).
    Hello {
        proto: u32,
        app_version: String,
        #[serde(default)]
        session: u64,
    },
    /// A message. `id` is a ULID chosen by the sender; `sent_at` is Unix milliseconds.
    Msg { id: String, sent_at: i64, text: String },
    /// "I've saved your message `id`."
    Ack { id: String },
    /// "I've read everything of yours up to and including `up_to`."
    Read { up_to: String },
    /// "My display name is now `name`."
    Name { name: String },
}

impl Frame {
    /// Whether the frame's fields are within the limits above. A peer that
    /// sends something outside them is broken or hostile.
    pub fn within_limits(&self) -> bool {
        match self {
            Frame::Hello { app_version, .. } => app_version.len() <= 64,
            Frame::Msg { id, text, .. } => id.len() <= MAX_ID && text.len() <= MAX_TEXT,
            Frame::Ack { id } | Frame::Read { up_to: id } => id.len() <= MAX_ID,
            Frame::Name { name } => name.len() <= 256,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes is over the {MAX_FRAME}-byte limit")]
    TooBig(usize),
    #[error("frame isn't valid: {0}")]
    Invalid(String),
    #[error("connection: {0}")]
    Connection(String),
}

/// The bytes on the wire for one frame.
pub fn encode(frame: &Frame) -> Vec<u8> {
    let json = serde_json::to_vec(frame).expect("a Frame always serializes");
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out
}

/// Parses one frame's JSON (without the length prefix).
pub fn decode(json: &[u8]) -> Result<Frame, FrameError> {
    let frame: Frame = serde_json::from_slice(json).map_err(|e| FrameError::Invalid(e.to_string()))?;
    if frame.within_limits() { Ok(frame) } else { Err(FrameError::Invalid("a field is too long".into())) }
}

pub async fn write_frame(send: &mut SendStream, frame: &Frame) -> Result<(), FrameError> {
    send.write_all(&encode(frame)).await.map_err(|e| FrameError::Connection(e.to_string()))
}

pub async fn read_frame(recv: &mut RecvStream) -> Result<Frame, FrameError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(|e| FrameError::Connection(e.to_string()))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooBig(len));
    }
    let mut json = vec![0u8; len];
    recv.read_exact(&mut json).await.map_err(|e| FrameError::Connection(e.to_string()))?;
    decode(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds() -> Vec<Frame> {
        vec![
            Frame::Hello { proto: PROTO_VERSION, app_version: "0.1.0".into(), session: 7 },
            Frame::Msg { id: "01J9ZQ4V6X3K8Y2M5N7P0R1S2T".into(), sent_at: 1_760_000_000_000, text: "hi 👋".into() },
            Frame::Ack { id: "01J9ZQ4V6X3K8Y2M5N7P0R1S2T".into() },
            Frame::Read { up_to: "01J9ZQ4V6X3K8Y2M5N7P0R1S2T".into() },
            Frame::Name { name: "rahul".into() },
        ]
    }

    #[test]
    fn every_frame_round_trips() {
        for frame in all_kinds() {
            let bytes = encode(&frame);
            let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            assert_eq!(len, bytes.len() - 4);
            assert_eq!(decode(&bytes[4..]).unwrap(), frame);
        }
    }

    #[test]
    fn the_wire_format_is_tagged_json() {
        let bytes = encode(&Frame::Ack { id: "x".into() });
        assert_eq!(&bytes[4..], br#"{"t":"ack","id":"x"}"#);
    }

    #[test]
    fn a_hello_from_before_sessions_is_still_read() {
        // As 0.1.0-rc.2 sends it.
        let old = br#"{"t":"hello","proto":1,"app_version":"0.1.0-rc.2"}"#;
        assert_eq!(decode(old).unwrap(), Frame::Hello { proto: 1, app_version: "0.1.0-rc.2".into(), session: 0 });
    }

    #[test]
    fn unknown_or_broken_frames_are_refused() {
        assert!(decode(br#"{"t":"selfdestruct"}"#).is_err());
        assert!(decode(br#"{"t":"msg","id":"x"}"#).is_err());
        assert!(decode(b"not json").is_err());
    }

    #[test]
    fn oversized_fields_are_refused() {
        let long_text = Frame::Msg { id: "x".into(), sent_at: 0, text: "a".repeat(MAX_TEXT + 1) };
        assert!(decode(&encode(&long_text)[4..]).is_err());
        let long_id = Frame::Ack { id: "a".repeat(MAX_ID + 1) };
        assert!(decode(&encode(&long_id)[4..]).is_err());
        let max_text = Frame::Msg { id: "x".into(), sent_at: 0, text: "a".repeat(MAX_TEXT) };
        assert!(decode(&encode(&max_text)[4..]).is_ok());
        assert!(encode(&max_text).len() <= MAX_FRAME);
    }
}
