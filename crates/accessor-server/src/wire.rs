//! The PostgreSQL frontend/backend protocol, version 3: framing and the little-endian-free (big-endian) encodings.

use std::io::{self, Read, Write};

/// A message being built: a type byte, then a length that is filled in by [`Msg::send`].
pub struct Msg {
    tag: u8,
    body: Vec<u8>,
}

impl Msg {
    pub fn new(tag: u8) -> Msg {
        Msg { tag, body: Vec::new() }
    }
    pub fn i16(mut self, v: i16) -> Msg {
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(mut self, v: i32) -> Msg {
        self.body.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn byte(mut self, v: u8) -> Msg {
        self.body.push(v);
        self
    }
    pub fn cstr(mut self, s: &str) -> Msg {
        self.body.extend_from_slice(s.as_bytes());
        self.body.push(0);
        self
    }
    pub fn bytes(mut self, b: &[u8]) -> Msg {
        self.body.extend_from_slice(b);
        self
    }
    /// A length-prefixed value, or -1 for NULL.
    pub fn value(mut self, v: Option<&[u8]>) -> Msg {
        match v {
            Some(b) => {
                self.body.extend_from_slice(&(b.len() as i32).to_be_bytes());
                self.body.extend_from_slice(b);
            }
            None => self.body.extend_from_slice(&(-1i32).to_be_bytes()),
        }
        self
    }
    pub fn send<W: Write + ?Sized>(self, w: &mut W) -> io::Result<()> {
        w.write_all(&[self.tag])?;
        w.write_all(&((self.body.len() + 4) as i32).to_be_bytes())?;
        w.write_all(&self.body)
    }
}

/// The body of a message being read.
pub struct Body<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "message is shorter than it says")
}

impl<'a> Body<'a> {
    pub fn new(buf: &'a [u8]) -> Body<'a> {
        Body { buf, pos: 0 }
    }
    pub fn i16(&mut self) -> io::Result<i16> {
        let b = self.buf.get(self.pos..self.pos + 2).ok_or_else(short)?;
        self.pos += 2;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i32(&mut self) -> io::Result<i32> {
        let b = self.buf.get(self.pos..self.pos + 4).ok_or_else(short)?;
        self.pos += 4;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn byte(&mut self) -> io::Result<u8> {
        let b = *self.buf.get(self.pos).ok_or_else(short)?;
        self.pos += 1;
        Ok(b)
    }
    pub fn cstr(&mut self) -> io::Result<String> {
        let rest = self.buf.get(self.pos..).ok_or_else(short)?;
        let end = rest.iter().position(|b| *b == 0).ok_or_else(short)?;
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.pos += end + 1;
        Ok(s)
    }
    pub fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let b = self.buf.get(self.pos..self.pos + n).ok_or_else(short)?;
        self.pos += n;
        Ok(b)
    }
    /// A length-prefixed value; `None` for NULL (length -1).
    pub fn value(&mut self) -> io::Result<Option<&'a [u8]>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize)?))
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let r = &self.buf[self.pos.min(self.buf.len())..];
        self.pos = self.buf.len();
        r
    }
}

/// The largest message accepted (a protection against a client that lies about its length).
const MAX_MESSAGE: usize = 1 << 28;

/// Reads one message after the startup: `(type, body)`. `Ok(None)` when the client closed the connection.
pub fn read_message<R: Read + ?Sized>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0u8; 1];
    match r.read(&mut tag)? {
        0 => return Ok(None),
        _ => {}
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = i32::from_be_bytes(len);
    if n < 4 || n as usize > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad message length"));
    }
    let mut body = vec![0u8; n as usize - 4];
    r.read_exact(&mut body)?;
    Ok(Some((tag[0], body)))
}

/// Reads a startup-phase packet, which has no type byte: `(first int32 after the length, rest)`.
pub fn read_startup<R: Read>(r: &mut R) -> io::Result<Option<(i32, Vec<u8>)>> {
    let mut len = [0u8; 4];
    match r.read(&mut len[..1])? {
        0 => return Ok(None),
        _ => r.read_exact(&mut len[1..])?,
    }
    let n = i32::from_be_bytes(len);
    if !(8..=10_000).contains(&n) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad startup packet length"));
    }
    let mut body = vec![0u8; n as usize - 4];
    r.read_exact(&mut body)?;
    let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    Ok(Some((code, body[4..].to_vec())))
}

pub const PROTOCOL_3: i32 = 196_608;
pub const SSL_REQUEST: i32 = 80_877_103;
pub const CANCEL_REQUEST: i32 = 80_877_102;
pub const GSS_REQUEST: i32 = 80_877_104;

/// An `ErrorResponse` / `NoticeResponse` body.
pub fn error_fields(tag: u8, severity: &str, code: &str, message: &str) -> Msg {
    Msg::new(tag).byte(b'S').cstr(severity).byte(b'V').cstr(severity).byte(b'C').cstr(code).byte(b'M').cstr(message).byte(0)
}
