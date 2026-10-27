//! The encoding a file declares, and nothing it does not.
//!
//! A bank's file is not always UTF-8: a Latin-1 or Windows-1251 camt.053 says so
//! in its XML declaration, and a UTF-16 one says so in its byte order mark.
//! Those two are statements by the sender and are honoured here: the bytes are
//! decoded to UTF-8 on the way in, so every reader above `Input` still sees one
//! encoding and nothing about the rest.
//!
//! What is not declared is not guessed. A file with no declaration is UTF-8,
//! which is what XML says it is, and an MT message declares no encoding at all.
//! Guessing a charset for either would be right most of the time and silently
//! wrong in the names it got wrong (`docs/adr/0010-a-declared-encoding-is-honoured.md`).
//!
//! The decode refuses to substitute. `Decoder::decode_to_utf8` would write U+FFFD
//! for a byte the encoding cannot read, and a counterparty name with a hole in it
//! is a changed fact in a payment file - what ADR 0001 forbids for an amount
//! holds for text. A malformed sequence, or one cut short at the end of the file,
//! is an error that names the file and the encoding.

use encoding_rs::{Decoder, DecoderResult, Encoding};
use std::io::{self, Read};

/// How far into a file a declaration is looked for. The XML declaration is the
/// first thing in a document and a few dozen bytes long.
const DECLARATION_BYTES: usize = 512;

/// Raw bytes held between a read of the file and the decode of them. Decoded
/// text goes into a buffer of the same size, so the memory a declared encoding
/// adds to a scan is these two and no more.
const STAGING_BYTES: usize = 8 * 1024;

/// What quick-xml says when an XML text node is not UTF-8, and what
/// `wire::record_subtree` says, in the same words, when a record subtree is not.
pub(crate) fn is_utf8_error(message: &str) -> bool {
    message.contains("Malformed UTF-8")
}

/// Appended to a UTF-8 error from a file that declared nothing. The text in
/// front stays the parser's own, so it is still what a caller searches for.
pub(crate) const UNDECLARED_HINT: &str =
    " - the file declares no encoding, and XML without a declaration is UTF-8";

/// The `encoding` the XML declaration at the start of `head` names, as written.
/// ASCII only: a declaration that can be read this way is in an ASCII-compatible
/// encoding, and a UTF-16 one is found by its byte order mark instead.
pub(crate) fn declaration_label(head: &[u8]) -> Option<&[u8]> {
    let head = &head[..head.len().min(DECLARATION_BYTES)];
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let decl = &head[..memchr::memmem::find(head, b"?>").unwrap_or(head.len())];
    let after = &decl[memchr::memmem::find(decl, b"encoding")? + b"encoding".len()..];
    let after = after
        .trim_ascii_start()
        .strip_prefix(b"=")?
        .trim_ascii_start();
    let quote = *after.first().filter(|q| matches!(q, b'"' | b'\''))?;
    let value = &after[1..];
    Some(&value[..value.iter().position(|b| *b == quote)?])
}

/// `Encoding` compares by address, and a `static` an `&Encoding` is taken from
/// can be duplicated by the compiler: `enc == UTF_8` was false for the very
/// encoding `for_label("utf-8")` returned. The name is the identity.
fn is(enc: &'static Encoding, other: &'static Encoding) -> bool {
    enc.name() == other.name()
}

/// The encoding a file declares when it is not UTF-8, and how many leading bytes
/// are a byte order mark rather than text. `None` is UTF-8: no declaration, a
/// UTF-8 one, or a UTF-8 BOM, and the file is then read exactly as it always was.
///
/// A byte order mark wins over a declaration, as XML specifies. An encoding the
/// declaration names that `encoding_rs` does not know is an error, because
/// reading the file as UTF-8 anyway is the guess this refuses to make.
pub(crate) fn declared(
    prefix: &[u8],
    name: &str,
) -> Result<Option<(&'static Encoding, usize)>, String> {
    if let Some((enc, bom)) = Encoding::for_bom(prefix) {
        return Ok((!is(enc, encoding_rs::UTF_8)).then_some((enc, bom)));
    }
    let Some(label) = declaration_label(prefix) else {
        return Ok(None);
    };
    let unsupported = || {
        format!(
            "{name}: declared encoding {:?} is not supported",
            String::from_utf8_lossy(label)
        )
    };
    let enc = Encoding::for_label(label).ok_or_else(unsupported)?;
    if is(enc, encoding_rs::UTF_8) {
        return Ok(None);
    }
    // UTF-16 and the `replacement` encodings decode to UTF-8 only: an ASCII
    // declaration of one is a file that is not what it says it is. A real UTF-16
    // file never reaches here, because it has a byte order mark.
    if !is(enc.output_encoding(), enc) {
        return Err(
            if is(enc, encoding_rs::UTF_16LE) || is(enc, encoding_rs::UTF_16BE) {
                format!(
                    "{} (a UTF-16 file starts with a byte order mark, and this one does not)",
                    unsupported()
                )
            } else {
                unsupported()
            },
        );
    }
    Ok(Some((enc, 0)))
}

/// A declared non-UTF-8 encoding turned into UTF-8, one 8 KiB step at a time.
pub(crate) struct Transcoder {
    encoding: &'static Encoding,
    decoder: Decoder,
    /// Bytes read from the file and not yet decoded: `raw[raw_at..raw_end]`.
    raw: Box<[u8]>,
    raw_at: usize,
    raw_end: usize,
    /// Text decoded and not yet handed out: `out[out_at..out_end]`.
    out: Box<[u8]>,
    out_at: usize,
    out_end: usize,
    /// The file has ended and the decoder has been told so.
    eof: bool,
    /// The decoder has flushed its last character: it must not be used again,
    /// and every read from here is the end of the text.
    done: bool,
    /// The decoder met a malformed sequence. A read after the error says so
    /// again, because the end of the text it would otherwise report is false.
    failed: bool,
}

impl Transcoder {
    pub(crate) fn new(encoding: &'static Encoding) -> Self {
        Transcoder {
            encoding,
            decoder: encoding.new_decoder_without_bom_handling(),
            raw: vec![0; STAGING_BYTES].into_boxed_slice(),
            raw_at: 0,
            raw_end: 0,
            out: vec![0; STAGING_BYTES].into_boxed_slice(),
            out_at: 0,
            out_end: 0,
            eof: false,
            done: false,
            failed: false,
        }
    }

    fn malformed(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bytes that are not valid {} - the file declares this encoding, \
                 so quackiso does not substitute characters",
                self.encoding.name()
            ),
        )
    }

    /// The decoded text of the prefix the guard read, all of it. A character the
    /// prefix ends inside stays in the decoder and completes on the next read.
    pub(crate) fn decode_prefix(&mut self, raw: &[u8]) -> io::Result<Vec<u8>> {
        let cap = self
            .decoder
            .max_utf8_buffer_length_without_replacement(raw.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "prefix too long"))?;
        let mut out = vec![0u8; cap];
        let (result, _, written) = self
            .decoder
            .decode_to_utf8_without_replacement(raw, &mut out, false);
        if matches!(result, DecoderResult::Malformed(..)) {
            return Err(self.malformed());
        }
        out.truncate(written);
        Ok(out)
    }

    /// Decode the next piece of `src` into `out`, returning when there is text
    /// to hand out or the file is over. Never leaves with nothing to give before
    /// the end: a raw read that ends inside a character decodes to nothing, and
    /// that is a reason to read again, not to report end of file.
    fn refill(&mut self, src: &mut impl Read) -> io::Result<()> {
        self.out_at = 0;
        self.out_end = 0;
        if self.failed {
            return Err(self.malformed());
        }
        while !self.done {
            if self.raw_at == self.raw_end && !self.eof {
                let n = src.read(&mut self.raw)?;
                self.raw_at = 0;
                self.raw_end = n;
                self.eof = n == 0;
            }
            let (result, read, written) = self.decoder.decode_to_utf8_without_replacement(
                &self.raw[self.raw_at..self.raw_end],
                &mut self.out,
                self.eof,
            );
            self.raw_at += read;
            self.out_end = written;
            if matches!(result, DecoderResult::Malformed(..)) {
                self.failed = true;
                return Err(self.malformed());
            }
            self.done = self.eof && result == DecoderResult::InputEmpty;
            if written > 0 {
                break;
            }
        }
        Ok(())
    }

    /// `Read::read` over the decoded text of `src`.
    pub(crate) fn read(&mut self, src: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.out_at == self.out_end {
            self.refill(src)?;
        }
        let n = buf.len().min(self.out_end - self.out_at);
        buf[..n].copy_from_slice(&self.out[self.out_at..self.out_at + n]);
        self.out_at += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_names_its_encoding() {
        for (head, label) in [
            (
                &b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a/>"[..],
                "ISO-8859-1",
            ),
            (
                b"<?xml version='1.0' encoding = 'windows-1251' ?>",
                "windows-1251",
            ),
            (b"<?xml encoding=\"UTF-8\" version=\"1.0\"?>", "UTF-8"),
        ] {
            assert_eq!(declaration_label(head), Some(label.as_bytes()), "{head:?}");
        }
        for head in [
            &b"<?xml version=\"1.0\"?><a encoding=\"x\"/>"[..],
            b"<a encoding=\"ISO-8859-1\"/>",
            b"<?xml version=\"1.0\" encoding=ISO-8859-1?>",
            b"<?xml version=\"1.0\" encoding=\"ISO-8859-1",
            b"",
        ] {
            assert_eq!(declaration_label(head), None, "{head:?}");
        }
    }

    #[test]
    fn only_a_non_utf8_declaration_asks_for_a_decoder() {
        let latin1 = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>";
        let (enc, bom) = declared(latin1, "f").unwrap().unwrap();
        assert_eq!((enc.name(), bom), ("windows-1252", 0));
        assert!(declared(b"<?xml version=\"1.0\" encoding=\"utf-8\"?>", "f")
            .unwrap()
            .is_none());
        assert!(declared(b"<?xml version=\"1.0\"?>", "f").unwrap().is_none());
        // a UTF-8 BOM is UTF-8, whatever the declaration after it says
        assert!(
            declared(b"\xEF\xBB\xBF<?xml encoding=\"ISO-8859-1\"?>", "f")
                .unwrap()
                .is_none()
        );
        let (enc, bom) = declared(b"\xFF\xFE<\0", "f").unwrap().unwrap();
        assert_eq!((enc.name(), bom), ("UTF-16LE", 2));
        let (enc, bom) = declared(b"\xFE\xFF\0<", "f").unwrap().unwrap();
        assert_eq!((enc.name(), bom), ("UTF-16BE", 2));
    }

    #[test]
    fn an_encoding_it_cannot_read_is_named_not_guessed() {
        let err = declared(b"<?xml version=\"1.0\" encoding=\"EBCDIC-9\"?>", "f.xml").unwrap_err();
        assert_eq!(
            err,
            "f.xml: declared encoding \"EBCDIC-9\" is not supported"
        );
        // UTF-16 in an ASCII declaration, with no byte order mark: the bytes are
        // 8-bit text, and decoding them as UTF-16 would be noise
        let err = declared(b"<?xml version=\"1.0\" encoding=\"UTF-16\"?>", "f.xml").unwrap_err();
        assert!(
            err.starts_with("f.xml: declared encoding \"UTF-16\" is not supported"),
            "{err}"
        );
    }

    /// A reader that hands out `step` bytes at a time, so a character can be
    /// split across any two reads.
    struct Trickle<'a>(&'a [u8], usize);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.1.min(buf.len()).min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    fn all_of(t: &mut Transcoder, raw: &[u8], step: usize, caller: usize) -> io::Result<String> {
        let mut src = Trickle(raw, step);
        let mut out = Vec::new();
        let mut buf = vec![0u8; caller];
        loop {
            match t.read(&mut src, &mut buf)? {
                0 => return Ok(String::from_utf8(out).unwrap()),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn a_character_split_across_raw_reads_arrives_whole() {
        let text = "Müller GmbH \u{20AC} \u{1F600} Ж";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for step in [1, 2, 3, 7] {
            let mut t = Transcoder::new(encoding_rs::UTF_16LE);
            assert_eq!(
                all_of(&mut t, &utf16, step, 4096).unwrap(),
                text,
                "step {step}"
            );
        }
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("Москва");
        let mut t = Transcoder::new(encoding_rs::WINDOWS_1251);
        assert_eq!(all_of(&mut t, &cp1251, 1, 4096).unwrap(), "Москва");
    }

    #[test]
    fn a_caller_buffer_shorter_than_a_character_still_makes_progress() {
        let text = "a\u{20AC}b\u{1F600}c";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut t = Transcoder::new(encoding_rs::UTF_16LE);
        assert_eq!(all_of(&mut t, &utf16, 5, 1).unwrap(), text);
    }

    #[test]
    fn a_long_stream_crosses_many_staging_buffers() {
        let text = "Müller ".repeat(10_000);
        let (latin1, _, _) = encoding_rs::WINDOWS_1252.encode(&text);
        let mut t = Transcoder::new(encoding_rs::WINDOWS_1252);
        assert_eq!(all_of(&mut t, &latin1, 4000, 64 * 1024).unwrap(), text);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_replacement() {
        // a lone high surrogate, then a character cut short at the end
        let mut t = Transcoder::new(encoding_rs::UTF_16LE);
        let err = all_of(&mut t, &[0x00, 0xD8, b'a', 0x00], 8192, 64).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-16LE"), "{err}");
        let mut t = Transcoder::new(encoding_rs::UTF_16LE);
        let err = all_of(&mut t, &[b'a', 0x00, b'b'], 8192, 64).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-16LE"), "{err}");
    }
}
