use std::fmt;
use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Request, Response};

/// Sanity bound on a single frame - these messages are small (a handful of
/// outputs, a version string), so anything claiming to be this large is
/// either a bug or a hostile peer, not a legitimate request/response.
const MAX_FRAME_LEN: usize = 1024 * 1024;

#[derive(Debug)]
pub enum CodecError {
    Io(io::Error),
    Postcard(postcard::Error),
    FrameTooLarge(usize),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::Io(e) => write!(f, "IPC I/O error: {e}"),
            CodecError::Postcard(e) => write!(f, "IPC decode error: {e}"),
            CodecError::FrameTooLarge(len) => write!(f, "IPC frame too large: {len} bytes"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<io::Error> for CodecError {
    fn from(e: io::Error) -> Self {
        CodecError::Io(e)
    }
}

impl From<postcard::Error> for CodecError {
    fn from(e: postcard::Error) -> Self {
        CodecError::Postcard(e)
    }
}

/// Writes one length-prefixed frame: a 4-byte little-endian length, then
/// that many bytes of payload.
pub fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<(), CodecError> {
    if bytes.len() > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge(bytes.len()));
    }
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

/// Reads one length-prefixed frame written by `write_frame`.
pub fn read_frame(stream: &mut impl Read) -> Result<Vec<u8>, CodecError> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len > MAX_FRAME_LEN {
        return Err(CodecError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    Ok(postcard::to_stdvec(value)?)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    Ok(postcard::from_bytes(bytes)?)
}

pub fn encode_request(req: &Request) -> Result<Vec<u8>, CodecError> {
    encode(req)
}

pub fn decode_request(bytes: &[u8]) -> Result<Request, CodecError> {
    decode(bytes)
}

pub fn encode_response(resp: &Response) -> Result<Vec<u8>, CodecError> {
    encode(resp)
}

pub fn decode_response(bytes: &[u8]) -> Result<Response, CodecError> {
    decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_through_a_byte_buffer() {
        let payload = b"hello frame".to_vec();
        let mut buf = Vec::new();
        write_frame(&mut buf, &payload).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let read_back = read_frame(&mut cursor).unwrap();
        assert_eq!(read_back, payload);
    }

    #[test]
    fn oversized_frame_is_rejected_on_write() {
        let huge = vec![0u8; MAX_FRAME_LEN + 1];
        assert!(matches!(write_frame(&mut Vec::new(), &huge), Err(CodecError::FrameTooLarge(_))));
    }

    #[test]
    fn request_round_trips_through_postcard() {
        for req in [Request::Outputs, Request::Version] {
            let bytes = encode_request(&req).unwrap();
            let decoded = decode_request(&bytes).unwrap();
            assert_eq!(format!("{decoded:?}"), format!("{req:?}"));
        }
    }

    #[test]
    fn output_response_round_trips_through_postcard() {
        let resp = Response::Outputs(crate::OutputsResponse {
            outputs: vec![crate::OutputInfo {
                name: "DP-1".to_string(),
                modes: vec![
                    crate::ModeInfo {
                        width: 2560,
                        height: 1440,
                        refresh_millihz: 179_998,
                        preferred: true,
                    },
                    crate::ModeInfo {
                        width: 1920,
                        height: 1080,
                        refresh_millihz: 60_000,
                        preferred: false,
                    },
                ],
                current_mode: Some(0),
                offset_x: 0,
                offset_y: 0,
                vrr: "auto".to_string(),
            }],
        });
        let bytes = encode_response(&resp).unwrap();
        let decoded = decode_response(&bytes).unwrap();
        assert_eq!(format!("{decoded:?}"), format!("{resp:?}"));
    }
}
