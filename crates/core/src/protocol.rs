use std::io::{Read, Write};
use std::net::TcpStream;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::{self, Identity};
use crate::model::{ShareAnn, WireFile};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Msg {
    #[serde(rename = "hello")]
    Hello {
        device_id: String,
        name: String,
        pubkey: String,
        proto: u32,
    },
    #[serde(rename = "pairRequest")]
    PairRequest,
    #[serde(rename = "pairAccept")]
    PairAccept,
    #[serde(rename = "pairReject")]
    PairReject,
    #[serde(rename = "shares")]
    Shares { shares: Vec<ShareAnn> },
    #[serde(rename = "index")]
    Index {
        share_id: String,
        entries: Vec<WireFile>,
    },
    #[serde(rename = "want")]
    Want { share_id: String },
    #[serde(rename = "get")]
    Get { share_id: String, rel_path: String },
    #[serde(rename = "file")]
    File {
        share_id: String,
        entry: WireFile,
    },
    #[serde(rename = "missing")]
    Missing { share_id: String, rel_path: String },
}

const MAX_FRAME: usize = 8 * 1024 * 1024;

pub fn write_frame(stream: &mut TcpStream, identity: &Identity, kind: u8, payload: &[u8]) -> Result<()> {
    let mut signed = Vec::with_capacity(1 + payload.len());
    signed.push(kind);
    signed.extend_from_slice(payload);
    let signature = identity.sign(&signed);
    let length = (signature.len() + signed.len()) as u32;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&signature)?;
    stream.write_all(&signed)?;
    stream.flush()?;
    Ok(())
}

pub fn write_msg(stream: &mut TcpStream, identity: &Identity, msg: &Msg) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    write_frame(stream, identity, 1, &payload)
}

pub fn read_frame(stream: &mut TcpStream) -> std::io::Result<Option<RawFrame>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(err) if is_timeout(&err) => return Ok(None),
        Err(err) => return Err(err),
    }
    let length = u32::from_be_bytes(len_buf) as usize;
    if length < 65 || length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad frame length",
        ));
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    let (sig_bytes, message) = body.split_at(64);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(sig_bytes);
    let kind = message[0];
    let payload = message[1..].to_vec();
    Ok(Some(RawFrame {
        kind,
        payload,
        signed: message.to_vec(),
        signature,
    }))
}

pub struct RawFrame {
    pub kind: u8,
    pub payload: Vec<u8>,
    pub signed: Vec<u8>,
    pub signature: [u8; 64],
}

impl RawFrame {
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        identity::verify(pubkey, &self.signed, &self.signature)
    }

    pub fn message(&self) -> Result<Msg> {
        if self.kind != 1 {
            return Err(Error::msg("ожидалось текстовое сообщение"));
        }
        Ok(serde_json::from_slice(&self.payload)?)
    }
}

pub fn encode_chunk(share_id: &str, rel_path: &str, offset: u64, total: u64, data: &[u8]) -> Result<Vec<u8>> {
    let share = share_id.as_bytes();
    let path = rel_path.as_bytes();
    if share.len() > u16::MAX as usize || path.len() > u16::MAX as usize {
        return Err(Error::msg("слишком длинное имя"));
    }
    let mut out = Vec::with_capacity(4 + share.len() + path.len() + 16 + data.len());
    out.extend_from_slice(&(share.len() as u16).to_be_bytes());
    out.extend_from_slice(share);
    out.extend_from_slice(&(path.len() as u16).to_be_bytes());
    out.extend_from_slice(path);
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

pub struct Chunk<'a> {
    pub share_id: &'a str,
    pub rel_path: &'a str,
    pub offset: u64,
    pub total: u64,
    pub data: &'a [u8],
}

pub fn decode_chunk(payload: &[u8]) -> Result<Chunk<'_>> {
    if payload.len() < 4 {
        return Err(Error::msg("короткий блок"));
    }
    let share_len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let rest = &payload[2..];
    if rest.len() < share_len + 2 {
        return Err(Error::msg("короткий блок"));
    }
    let share = std::str::from_utf8(&rest[..share_len]).map_err(|_| Error::msg("путь не utf-8"))?;
    let rest = &rest[share_len..];
    let path_len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
    let rest = &rest[2..];
    if rest.len() < path_len + 16 {
        return Err(Error::msg("короткий блок"));
    }
    let path = std::str::from_utf8(&rest[..path_len]).map_err(|_| Error::msg("путь не utf-8"))?;
    let rest = &rest[path_len..];
    let offset = u64::from_be_bytes(rest[0..8].try_into().unwrap());
    let total = u64::from_be_bytes(rest[8..16].try_into().unwrap());
    Ok(Chunk {
        share_id: share,
        rel_path: path,
        offset,
        total,
        data: &rest[16..],
    })
}

fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_roundtrip() {
        let bytes = encode_chunk("share", "a/b.txt", 5, 9, b"xyz").unwrap();
        let chunk = decode_chunk(&bytes).unwrap();
        assert_eq!(chunk.share_id, "share");
        assert_eq!(chunk.rel_path, "a/b.txt");
        assert_eq!(chunk.offset, 5);
        assert_eq!(chunk.total, 9);
        assert_eq!(chunk.data, b"xyz");
    }
}
