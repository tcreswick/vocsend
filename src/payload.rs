//! What is carried inside the blocks: an optional file name and the data,
//! compressed if that makes it smaller.
//!
//!   byte 0   bit 0: the data is compressed; bit 1: a file name follows
//!   [1 length byte, then the name in UTF-8]
//!   the data

use crate::pack;

pub fn wrap(data: &[u8], name: Option<&str>) -> Vec<u8> {
    let packed = pack::compress(data);
    let squeeze = packed.len() < data.len();
    let label: &[u8] = name.map(|n| n.as_bytes()).unwrap_or(&[]);
    let label = &label[..label.len().min(255)];
    let mut out = vec![squeeze as u8 | ((!label.is_empty()) as u8) << 1];
    if !label.is_empty() {
        out.push(label.len() as u8);
        out.extend_from_slice(label);
    }
    out.extend_from_slice(if squeeze { &packed } else { data });
    out
}

/// Bytes of `stream` taken up by the flags and name.
pub fn header_len(stream: &[u8]) -> usize {
    match stream.first() {
        Some(f) if f & 2 != 0 => 2 + stream.get(1).copied().unwrap_or(0) as usize,
        _ => 1,
    }
}

pub fn is_compressed(stream: &[u8]) -> bool {
    stream.first().is_some_and(|f| f & 1 != 0)
}

/// (file name if one was sent, data) from a complete stream.
pub fn unwrap(stream: &[u8]) -> Result<(Option<String>, Vec<u8>), &'static str> {
    let flags = *stream.first().ok_or("nothing arrived")?;
    let head = header_len(stream);
    if stream.len() < head {
        return Err("the data ended inside its header");
    }
    let name = (flags & 2 != 0).then(|| String::from_utf8_lossy(&stream[2..head]).into_owned());
    let body = &stream[head..];
    let data = if flags & 1 != 0 {
        pack::decompress(body).ok_or("the compressed data is corrupt")?
    } else {
        body.to_vec()
    };
    Ok((name, data))
}

/// A received file name, reduced to something safe to create in the
/// current directory: no path, no control characters, not too long.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let (stem, ext) = match clean.rfind('.') {
        Some(i) if i > 0 => clean.split_at(i),
        _ => (clean.as_str(), ""),
    };
    let short: String = stem.chars().take(150).chain(ext.chars().take(20)).collect();
    if short.is_empty() || short == "." || short == ".." {
        "received.bin".to_string()
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps() {
        let text = b"hello hello hello hello hello hello hello hello hello hello".to_vec();
        let stream = wrap(&text, Some("notes.txt"));
        assert!(is_compressed(&stream));
        assert_eq!(unwrap(&stream).unwrap(), (Some("notes.txt".to_string()), text));
        let random: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(97) ^ 0x5c).collect();
        let stream = wrap(&random, None);
        assert_eq!(header_len(&stream), 1);
        assert_eq!(unwrap(&stream).unwrap(), (None, random));
        assert_eq!(unwrap(&wrap(b"", None)).unwrap(), (None, vec![]));
        assert!(unwrap(&[]).is_err());
        assert!(unwrap(&[2, 9, b'a']).is_err());
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Users\\me\\key.pub"), "key.pub");
        assert_eq!(safe_name(".."), "received.bin");
        assert_eq!(safe_name(""), "received.bin");
        assert_eq!(safe_name("a\nb.txt"), "ab.txt");
        assert!(safe_name(&"x".repeat(400)).len() <= 170);
        assert_eq!(safe_name(".bashrc"), ".bashrc");
    }
}
