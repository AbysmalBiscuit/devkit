//! Newline-delimited JSON framing for the daemon control channel. Generic over
//! any serde message type so each registry's proto reuses it.

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{BufRead, Write};

/// Write one newline-delimited JSON frame and flush.
pub fn send<W: Write>(w: &mut W, msg: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one newline-delimited JSON frame. `Ok(None)` on clean EOF.
pub fn recv<R: BufRead, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(s.trim_end())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_over_a_pipe() {
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &("hello", 7u32)).unwrap();
        assert_eq!(*buf.last().unwrap(), b'\n');
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: (String, u32) = recv(&mut rdr).unwrap().expect("one frame");
        assert_eq!(back, ("hello".to_string(), 7));
    }

    #[test]
    fn recv_returns_none_on_eof() {
        let mut rdr = std::io::BufReader::new(&b""[..]);
        let got: Option<(String, u32)> = recv(&mut rdr).unwrap();
        assert!(got.is_none());
    }
}
