//! Snapcast's binary protocol, the client's side: <https://github.com/badaix/snapcast/blob/master/doc/binary_protocol.md>.
//! Every message is a 26-byte little-endian header followed by its typed body.

use anyhow::{bail, Context};
use serde::Deserialize;
use std::io::{Read, Write};

pub const HEADER_SIZE: usize = 26;

const CODEC_HEADER: u16 = 1;
const WIRE_CHUNK: u16 = 2;
const SERVER_SETTINGS: u16 = 3;
const TIME: u16 = 4;
const HELLO: u16 = 5;
const CLIENT_INFO: u16 = 7;
const ERROR: u16 = 8;

/// A timestamp in microseconds, sent as whole seconds and the microseconds past them
pub type Micros = i64;

fn split(us: Micros) -> (i32, i32) {
    let sec = us.div_euclid(1_000_000);
    let usec = us.rem_euclid(1_000_000);
    (sec as i32, usec as i32)
}

fn join(sec: i32, usec: i32) -> Micros {
    i64::from(sec) * 1_000_000 + i64::from(usec)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u16,
    pub id: u16,
    pub refers_to: u16,
    pub sent: Micros,
    pub received: Micros,
    pub size: u32,
}

impl Header {
    pub fn parse(b: &[u8; HEADER_SIZE]) -> Self {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let i32_at = |i: usize| i32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Self {
            kind: u16_at(0),
            id: u16_at(2),
            refers_to: u16_at(4),
            sent: join(i32_at(6), i32_at(10)),
            received: join(i32_at(14), i32_at(18)),
            size: u32::from_le_bytes([b[22], b[23], b[24], b[25]]),
        }
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        let (sent_sec, sent_usec) = split(self.sent);
        let (recv_sec, recv_usec) = split(self.received);
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.refers_to.to_le_bytes());
        for v in [sent_sec, sent_usec, recv_sec, recv_usec] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.size.to_le_bytes());
    }
}

/// The PCM layout the server streams in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    pub buffer_ms: i64,
    pub latency: i64,
    pub muted: bool,
    pub volume: u8,
}

#[derive(Debug, PartialEq)]
pub enum Message {
    CodecHeader(Format),
    WireChunk {
        timestamp: Micros,
        payload: Vec<u8>,
    },
    ServerSettings(ServerSettings),
    /// The server's answer to a time request: client-to-server latency as the server measured it
    Time {
        latency: Micros,
    },
    Error(String),
    Other(u16),
}

/// Read one message and the header it came with; `received` is stamped with `now`
pub fn read(r: &mut impl Read, now: impl Fn() -> Micros) -> anyhow::Result<(Header, Message)> {
    let mut head = [0u8; HEADER_SIZE];
    r.read_exact(&mut head)?;
    let mut header = Header::parse(&head);
    header.received = now();
    let mut body = vec![0u8; header.size as usize];
    r.read_exact(&mut body)?;
    let message = parse_body(header.kind, &body)
        .with_context(|| format!("parse a message of type {}", header.kind))?;
    Ok((header, message))
}

fn parse_body(kind: u16, body: &[u8]) -> anyhow::Result<Message> {
    let mut cur = Cursor { buf: body, pos: 0 };
    Ok(match kind {
        CODEC_HEADER => {
            let codec = String::from_utf8_lossy(cur.sized()?).into_owned();
            if codec != "pcm" {
                bail!("unsupported codec {codec}: the server must stream pcm");
            }
            Message::CodecHeader(parse_wav_header(cur.sized()?)?)
        }
        WIRE_CHUNK => {
            let sec = cur.i32()?;
            let usec = cur.i32()?;
            Message::WireChunk {
                timestamp: join(sec, usec),
                payload: cur.sized()?.to_vec(),
            }
        }
        SERVER_SETTINGS => Message::ServerSettings(serde_json::from_slice(cur.sized()?)?),
        TIME => {
            let sec = cur.i32()?;
            let usec = cur.i32()?;
            Message::Time {
                latency: join(sec, usec),
            }
        }
        ERROR => {
            let _code = cur.u32()?;
            let error = String::from_utf8_lossy(cur.sized()?).into_owned();
            let detail = String::from_utf8_lossy(cur.sized()?).into_owned();
            Message::Error(format!("{error}: {detail}"))
        }
        other => Message::Other(other),
    })
}

/// A RIFF WAVE header's sample rate, channel count and bit depth
fn parse_wav_header(b: &[u8]) -> anyhow::Result<Format> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("not a RIFF WAVE header");
    }
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        let data = &b[pos + 8..];
        if id == b"fmt " && data.len() >= 16 {
            return Ok(Format {
                channels: u16::from_le_bytes([data[2], data[3]]),
                rate: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
                bits: u16::from_le_bytes([data[14], data[15]]),
            });
        }
        pos += 8 + len;
    }
    bail!("the WAVE header has no fmt chunk")
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        let end = self.pos + n;
        let Some(slice) = self.buf.get(self.pos..end) else {
            bail!("message ends early");
        };
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> anyhow::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> anyhow::Result<i32> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length-prefixed field
    fn sized(&mut self) -> anyhow::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

fn frame(kind: u16, id: u16, sent: Micros, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + body.len());
    Header {
        kind,
        id,
        refers_to: 0,
        sent,
        received: 0,
        size: body.len() as u32,
    }
    .write_to(&mut out);
    out.extend_from_slice(body);
    out
}

fn json_body(value: &serde_json::Value) -> Vec<u8> {
    let json = value.to_string();
    let mut body = Vec::with_capacity(4 + json.len());
    body.extend_from_slice(&(json.len() as u32).to_le_bytes());
    body.extend_from_slice(json.as_bytes());
    body
}

pub fn hello(id: &str, host: &str, now: Micros) -> Vec<u8> {
    let body = json_body(&serde_json::json!({
        "Arch": std::env::consts::ARCH,
        "ClientName": "spotify_player",
        "HostName": host,
        "ID": id,
        "Instance": 1,
        "MAC": "00:00:00:00:00:00",
        "OS": std::env::consts::OS,
        "SnapStreamProtocolVersion": 2,
        "Version": env!("CARGO_PKG_VERSION"),
    }));
    frame(HELLO, 0, now, &body)
}

pub fn time_request(id: u16, now: Micros) -> Vec<u8> {
    frame(TIME, id, now, &[0u8; 8])
}

pub fn client_info(muted: bool, now: Micros) -> Vec<u8> {
    let body = json_body(&serde_json::json!({ "volume": 100, "muted": muted }));
    frame(CLIENT_INFO, 0, now, &body)
}

pub fn send(w: &mut impl Write, message: &[u8]) -> std::io::Result<()> {
    w.write_all(message)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_header(rate: u32, channels: u16, bits: u16) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        b.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data\0\0\0\0");
        b
    }

    fn sized(b: &[u8]) -> Vec<u8> {
        let mut out = (b.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(b);
        out
    }

    #[test]
    fn header_round_trips_negative_and_large_times() {
        let header = Header {
            kind: TIME,
            id: 7,
            refers_to: 3,
            sent: -1_500_000,
            received: 1_761_000_000_123_456,
            size: 8,
        };
        let mut out = Vec::new();
        header.write_to(&mut out);
        assert_eq!(out.len(), HEADER_SIZE);
        let parsed = Header::parse(out.as_slice().try_into().unwrap());
        assert_eq!(parsed.sent, -1_500_000);
        assert_eq!(parsed.received, header.received);
        assert_eq!((parsed.kind, parsed.id, parsed.refers_to), (TIME, 7, 3));
    }

    #[test]
    fn reads_a_pcm_codec_header() {
        let mut body = sized(b"pcm");
        body.extend(sized(&wav_header(44_100, 2, 16)));
        let msg = frame(CODEC_HEADER, 0, 0, &body);
        let (_, parsed) = read(&mut msg.as_slice(), || 42).unwrap();
        assert_eq!(
            parsed,
            Message::CodecHeader(Format {
                rate: 44_100,
                channels: 2,
                bits: 16
            })
        );
    }

    #[test]
    fn refuses_a_codec_other_than_pcm() {
        let mut body = sized(b"flac");
        body.extend(sized(b"fLaC"));
        let msg = frame(CODEC_HEADER, 0, 0, &body);
        assert!(read(&mut msg.as_slice(), || 0).is_err());
    }

    #[test]
    fn reads_a_wire_chunk_and_stamps_its_arrival() {
        let mut body = 12i32.to_le_bytes().to_vec();
        body.extend_from_slice(&345_678i32.to_le_bytes());
        body.extend(sized(&[1, 2, 3, 4]));
        let msg = frame(WIRE_CHUNK, 0, 5, &body);
        let (header, parsed) = read(&mut msg.as_slice(), || 99).unwrap();
        assert_eq!(header.received, 99);
        assert_eq!(header.sent, 5);
        assert_eq!(
            parsed,
            Message::WireChunk {
                timestamp: 12_345_678,
                payload: vec![1, 2, 3, 4]
            }
        );
    }

    #[test]
    fn reads_server_settings() {
        let body = json_body(&serde_json::json!({
            "bufferMs": 600, "latency": 0, "muted": true, "volume": 80
        }));
        let msg = frame(SERVER_SETTINGS, 0, 0, &body);
        let (_, parsed) = read(&mut msg.as_slice(), || 0).unwrap();
        assert_eq!(
            parsed,
            Message::ServerSettings(ServerSettings {
                buffer_ms: 600,
                latency: 0,
                muted: true,
                volume: 80
            })
        );
    }
}
