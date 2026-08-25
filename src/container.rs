//! Transport containers, named rather than parsed.
//!
//! A bank does not always hand over a message. It hands over the day's
//! statements in one ZIP, a signed camt.053 wrapped in a PKCS#7 envelope, a
//! pain.001 encrypted to a PGP key, or the whole exchange inside an EBICS
//! request. None of those is an ISO 20022 message and none of them is SWIFT MT,
//! but every one of them contains bytes that look like one: a ZIP of two
//! statements holds `<` and `:20:` alike, so the existing MT-before-markup
//! precedence answers the wrong question about it.
//!
//! Before this, a TAR of two camt.053 documents parsed as one file and returned
//! the entries of both members under one `source_file` - two statements added
//! together with nothing on the row saying so. That is the failure this module
//! refuses by name. It reads no members: naming the container is the honest
//! answer, and an archive-member reader is a separate design with its own grain.
//!
//! Every check is bounded by the prefix it is handed - one `Source` buffer, the
//! same 64 KiB `shape_of` decides everything else on - and none of them
//! decompresses, decrypts, or allocates according to a length the file states.
//! A length longer than the prefix is ordinary: the prefix is the first 64 KiB
//! of a file that may be gigabytes, so a structural check reads what is there
//! and refuses to trust the rest.
//!
//! The near misses are the point. A magic string alone names nothing here: a
//! `ustar` inside an ordinary XML document is not a TAR without a header
//! checksum that agrees, a DER SEQUENCE is not CMS without the content-type
//! arc, and one OpenPGP packet-tag byte is not an envelope without a packet
//! chain whose body versions exist. What each check costs when it is wrong is
//! a file refused that a reader could have read, so each of them requires
//! structure and not a substring.

/// A container quackiso names and refuses. Gzip is not among them: a gzipped
/// message is still one message, and `Source` unwraps it before any of this is
/// reached - which is also why a `.tar.gz` is caught as TAR.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ContainerKind {
    Zip,
    Tar,
    Pkcs7,
    Pgp,
    Ebics,
}

impl ContainerKind {
    /// What to do about it. One sentence per kind, shared by the sniffer's
    /// `error` column and by every reader's raise, so a caller cannot get two
    /// different accounts of the same file.
    pub fn reason(self) -> &'static str {
        match self {
            ContainerKind::Zip => "ZIP archive: extract a member before reading",
            ContainerKind::Tar => "TAR archive: extract a member before reading",
            ContainerKind::Pkcs7 => "PKCS#7 envelope: unwrap, decrypt, or verify it before reading",
            ContainerKind::Pgp => "PGP envelope: decrypt it before reading",
            ContainerKind::Ebics => {
                "EBICS transport envelope: process it with an EBICS client first"
            }
        }
    }
}

/// Which container the prefix is, if it is one. Ordered cheapest first; the
/// kinds do not overlap, because each requires structure the others do not
/// have.
pub fn detect(prefix: &[u8]) -> Option<ContainerKind> {
    if is_zip(prefix) {
        return Some(ContainerKind::Zip);
    }
    if is_tar(prefix) {
        return Some(ContainerKind::Tar);
    }
    if is_pkcs7(prefix) {
        return Some(ContainerKind::Pkcs7);
    }
    if is_pgp(prefix) {
        return Some(ContainerKind::Pgp);
    }
    if is_ebics(prefix) {
        return Some(ContainerKind::Ebics);
    }
    None
}

// ── ZIP ──────────────────────────────────────────────────────────────────────

/// The six record signatures a ZIP file can open with: a member, an empty
/// archive, a spanned archive's marker, the temporary spanning marker the first
/// segment of a split set carries, and the two ZIP64 end records a large
/// archive ends with and can begin with when it holds no members.
///
/// Byte zero is where this looks, so a self-extracting archive behind an
/// executable stub is not found here. That one degrades to `not XML` rather
/// than to merged rows, because a PE stub is not parseable markup.
const ZIP_SIGNATURES: [&[u8; 4]; 6] = [
    b"PK\x03\x04",
    b"PK\x05\x06",
    b"PK\x07\x08",
    b"PK\x06\x06",
    b"PK\x06\x07",
    b"PK00",
];

fn is_zip(prefix: &[u8]) -> bool {
    prefix.len() >= 4
        && ZIP_SIGNATURES
            .iter()
            .any(|sig| &prefix[..4] == sig.as_slice())
}

// ── TAR ──────────────────────────────────────────────────────────────────────

const TAR_BLOCK: usize = 512;
/// Where the header states its own checksum, and the range the checksum is
/// computed with treated as spaces.
const TAR_CHECKSUM: std::ops::Range<usize> = 148..156;
const TAR_SIZE: std::ops::Range<usize> = 124..136;
const TAR_TYPE_FLAG: usize = 156;

/// A complete first header block whose stored checksum agrees with the bytes.
///
/// The checksum is the whole check. `ustar` at offset 257 is corroboration and
/// not a requirement - a v7 tar has no magic at all and is still a tar - and it
/// is not sufficient either: an XML document that quotes the word `ustar` in a
/// comment lands at that offset by accident often enough to matter.
fn is_tar(prefix: &[u8]) -> bool {
    let Some(header) = prefix.get(..TAR_BLOCK) else {
        return false;
    };
    let Some(stored) = tar_octal(&header[TAR_CHECKSUM]) else {
        return false;
    };
    header[0] != 0
        && tar_size_field(&header[TAR_SIZE])
        && tar_type_flag(header[TAR_TYPE_FLAG])
        && (stored == tar_checksum(header, false) || stored == tar_checksum(header, true))
}

/// The sum of the 512 header bytes, with the checksum field itself read as
/// eight spaces - which is how the writer computed the value it stored.
///
/// Both sums, because implementations that summed with a signed `char` stored a
/// different value for any header carrying a byte at or above 0x80 - a UTF-8
/// member name, or a GNU base-256 numeric field. GNU tar's own reader accepts
/// either, and accepting only one leaves those archives parsed as a document.
fn tar_checksum(header: &[u8], signed: bool) -> i64 {
    header
        .iter()
        .enumerate()
        .map(|(i, byte)| match TAR_CHECKSUM.contains(&i) {
            true => i64::from(b' '),
            false if signed => i64::from(*byte as i8),
            false => i64::from(*byte),
        })
        .sum()
}

/// A tar numeric field: leading spaces, octal digits, then NUL or space
/// padding. Anything else is not a number, which is what tells a header from
/// bytes that happen to be 512 long.
fn tar_octal(field: &[u8]) -> Option<i64> {
    let mut value: i64 = 0;
    let mut digits = 0usize;
    let mut at = 0usize;
    while field.get(at) == Some(&b' ') {
        at += 1;
    }
    while let Some(byte) = field.get(at) {
        match byte {
            b'0'..=b'7' => {
                value = value.checked_mul(8)?.checked_add(i64::from(byte - b'0'))?;
                digits += 1;
            }
            0 | b' ' => break,
            _ => return None,
        }
        at += 1;
    }
    match field[at..].iter().all(|byte| matches!(byte, 0 | b' ')) {
        true => (digits > 0).then_some(value),
        false => None,
    }
}

/// GNU writes a size past 8 GB as base 256 with the high bit set on the first
/// byte. The value is not wanted here, only that the field is one of the two
/// encodings a writer produces.
fn tar_size_field(field: &[u8]) -> bool {
    field.first().is_some_and(|byte| byte & 0x80 != 0) || tar_octal(field).is_some()
}

/// The POSIX file types, the pax extended headers (`x`, `g`), and the GNU
/// extensions: long name and link (`L`, `K`), sparse (`S`), volume header
/// (`V`), multi-volume continuation (`M`), directory dump (`D`), and the two
/// incremental-dump entries (`N`). NUL is the v7 spelling of a plain file.
fn tar_type_flag(flag: u8) -> bool {
    matches!(
        flag,
        0 | b'0'..=b'7' | b'x' | b'g' | b'L' | b'K' | b'S' | b'V' | b'M' | b'D' | b'N'
    )
}

// ── PKCS#7 / CMS ─────────────────────────────────────────────────────────────

/// `1.2.840.113549.1.7` - the arc every CMS content type sits under - as the
/// bytes an object identifier encodes it to. A `signedData` is `...1.7.2`, an
/// `envelopedData` `...1.7.3`, so the arc plus at least one more byte is the
/// whole family without listing it.
const CMS_ARC: [u8; 8] = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07];

/// An envelope is what the whole file is, so the armor has to open the file.
///
/// This was three unanchored searches over 64 KiB, which is a substring test
/// wearing a structure test's clothes: a camt.053 that carries its own detached
/// signature in `<SplmtryData><Envlp><Sgntr>`, and free text reading
/// `MIGRATION BEGIN CMS PHASE 2`, were both refused as envelopes. Neither is
/// one. RFC 7468 puts the boundary on a line of its own and a file that *is* an
/// envelope has nothing above it, so the anchor costs no true positive.
///
/// S/MIME states the type in a header rather than in armor, so that one is
/// looked for in the header block and not in the body it describes.
fn is_pkcs7(prefix: &[u8]) -> bool {
    document_starts_with(prefix, b"-----BEGIN PKCS7-----")
        || document_starts_with(prefix, b"-----BEGIN CMS-----")
        || contains_ascii_case(mime_headers(prefix), b"application/pkcs7-mime")
        || der_content_info(prefix)
}

/// A DER `ContentInfo`: a SEQUENCE whose first child is a content-type OID
/// under the CMS arc. Structural rather than a byte search, because those nine
/// OID bytes occur inside every certificate a signature carries and inside
/// plenty of DER that is not an envelope at all.
fn der_content_info(prefix: &[u8]) -> bool {
    let Some(body) = Der(prefix).tagged(0x30) else {
        return false;
    };
    let Some(oid) = Der(body).tagged(0x06) else {
        return false;
    };
    oid.len() > CMS_ARC.len() && oid.starts_with(&CMS_ARC)
}

/// One step of DER, over whatever bytes are in hand.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    /// The contents of the leading TLV when it carries `tag`.
    ///
    /// A stated length past the end of the prefix is clamped rather than
    /// refused: the outer SEQUENCE of a signed statement is longer than 64 KiB
    /// as a matter of course, and only its first child is being read. Nothing
    /// is allocated for the stated length, so a four-byte length of 0xFFFFFFFF
    /// costs one comparison.
    fn tagged(&mut self, tag: u8) -> Option<&'a [u8]> {
        let bytes = self.0;
        if *bytes.first()? != tag {
            return None;
        }
        let first = *bytes.get(1)?;
        let (start, len) = match first {
            // indefinite length: the contents run to the end-of-contents
            // octets, which a prefix need not hold
            0x80 => (2, bytes.len() - 2),
            n if n < 0x80 => (2, usize::from(n)),
            n => {
                let count = usize::from(n & 0x7F);
                if count == 0 || count > 4 {
                    return None;
                }
                let raw = bytes.get(2..2 + count)?;
                let len = raw
                    .iter()
                    .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
                (2 + count, len)
            }
        };
        let end = start.checked_add(len)?.min(bytes.len());
        self.0 = &bytes[end..];
        Some(&bytes[start..end])
    }
}

// ── OpenPGP ──────────────────────────────────────────────────────────────────

/// An encrypted message, in the armor a mail client writes.
const PGP_ARMOR: &[u8] = b"-----BEGIN PGP MESSAGE-----";

fn is_pgp(prefix: &[u8]) -> bool {
    // The same rule as the PEM armor above, and for the same reason: a message
    // that embeds an encrypted attachment on a line of its own is a message
    // with an attachment, not an envelope.
    document_starts_with(prefix, PGP_ARMOR) || pgp_packets(prefix)
}

/// Packet tags, from RFC 9580 section 5.
const PKESK: u8 = 1;
const SKESK: u8 = 3;
/// The pre-2007 symmetrically encrypted data packet: no version octet at all.
const SED: u8 = 9;
const SEIPD: u8 = 18;

/// A binary OpenPGP message: session-key packets and then the encrypted data,
/// or the encrypted data alone.
///
/// The packet chain is walked because a tag byte proves nothing. `0xC3` is a
/// well-formed new-format SKESK header and also an ordinary byte in the middle
/// of anything; what an envelope has that noise does not is a header whose
/// length can be followed to the next header, and a body whose first octet is a
/// version that exists.
fn pgp_packets(prefix: &[u8]) -> bool {
    let mut at = 0usize;
    let mut keys = 0usize;
    loop {
        let Some(packet) = pgp_header(prefix, at) else {
            return false;
        };
        let version = prefix.get(packet.body).copied();
        match packet.tag {
            PKESK | SKESK => {
                let known = match (packet.tag, version) {
                    (PKESK, Some(v)) => matches!(v, 3 | 6),
                    (SKESK, Some(v)) => matches!(v, 4..=6),
                    _ => false,
                };
                let Some(next) = packet.next else {
                    // a key packet has a definite length in every edition; a
                    // partial or indeterminate one is not this shape
                    return false;
                };
                if !known {
                    return false;
                }
                keys += 1;
                at = next;
            }
            // The legacy packet carries no version, so the key packets ahead of
            // it are what says this is an envelope rather than a coincidence.
            SED => return keys > 0,
            SEIPD => return matches!(version, Some(1 | 2)),
            _ => return false,
        }
    }
}

/// One OpenPGP packet header: what it is, where its body starts, and where the
/// next header starts when the length says.
struct PgpPacket {
    tag: u8,
    body: usize,
    /// `None` for a partial or indeterminate length, which cannot be stepped
    /// over without reading the body.
    next: Option<usize>,
}

fn pgp_header(prefix: &[u8], at: usize) -> Option<PgpPacket> {
    let first = *prefix.get(at)?;
    if first & 0x80 == 0 {
        return None;
    }
    let (tag, body, len) = match first & 0x40 != 0 {
        // RFC 4880 onwards: six tag bits and a self-describing length
        true => {
            let tag = first & 0x3F;
            match *prefix.get(at + 1)? {
                n @ 0..=191 => (tag, at + 2, Some(usize::from(n))),
                n @ 192..=223 => {
                    let second = *prefix.get(at + 2)?;
                    let len = ((usize::from(n) - 192) << 8) + usize::from(second) + 192;
                    (tag, at + 3, Some(len))
                }
                // a partial body length: the body continues in further chunks
                224..=254 => (tag, at + 2, None),
                255 => {
                    let raw = prefix.get(at + 2..at + 6)?;
                    let len = raw
                        .iter()
                        .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
                    (tag, at + 6, Some(len))
                }
            }
        }
        // the original format: four tag bits and a length type
        false => {
            let tag = (first >> 2) & 0x0F;
            match first & 0x03 {
                0 => (tag, at + 2, Some(usize::from(*prefix.get(at + 1)?))),
                1 => {
                    let raw = prefix.get(at + 1..at + 3)?;
                    let len = (usize::from(raw[0]) << 8) | usize::from(raw[1]);
                    (tag, at + 3, Some(len))
                }
                2 => {
                    let raw = prefix.get(at + 1..at + 5)?;
                    let len = raw
                        .iter()
                        .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
                    (tag, at + 5, Some(len))
                }
                // indeterminate: the packet runs to the end of the file
                _ => (tag, at + 1, None),
            }
        }
    };
    Some(PgpPacket {
        tag,
        body,
        next: len.and_then(|n| body.checked_add(n)),
    })
}

// ── EBICS ────────────────────────────────────────────────────────────────────

/// The two namespace roots EBICS binds: the versioned schemas, and the
/// unversioned one H000 and HEV declare.
const EBICS_NAMESPACES: [&[u8]; 2] = [b"urn:org:ebics:", b"http://www.ebics.org/"];

/// An EBICS transport envelope: a root element whose local name opens `ebics`
/// and which binds an EBICS namespace on that same tag.
///
/// Both halves are required. The local name alone would refuse a national
/// message that happens to be spelled that way, and a namespace alone would
/// refuse an ISO 20022 message an EBICS client had already unwrapped and left
/// the declaration on.
///
/// A root tag that does not finish inside the prefix is not classified at all,
/// so the file falls through to ordinary XML - a 64 KiB start tag is not an
/// EBICS envelope, and guessing at one would refuse a document nobody can see
/// the end of.
fn is_ebics(prefix: &[u8]) -> bool {
    let Some(tag) = root_start_tag(prefix) else {
        return false;
    };
    let split = tag
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b'/')
        .unwrap_or(tag.len());
    let (name, attributes) = tag.split_at(split);
    let local = match name.iter().rposition(|byte| *byte == b':') {
        Some(colon) => &name[colon + 1..],
        None => name,
    };
    local.starts_with(b"ebics") && has_ebics_namespace(attributes)
}

/// The first element start tag of the prefix, angle brackets removed. XML
/// declarations, processing instructions, comments and a doctype - internal
/// subset and all - are stepped over.
fn root_start_tag(prefix: &[u8]) -> Option<&[u8]> {
    let mut at = 0usize;
    loop {
        at += prefix.get(at..)?.iter().position(|byte| *byte == b'<')?;
        let rest = prefix.get(at..)?;
        if rest.starts_with(b"<!--") {
            at += 4 + find(rest.get(4..)?, b"-->")? + 3;
        } else if rest.starts_with(b"<?") {
            at += 2 + find(rest.get(2..)?, b"?>")? + 2;
        } else if rest.starts_with(b"<!") {
            at += 2 + doctype_end(rest.get(2..)?)?;
        } else if rest.starts_with(b"</") {
            // a close tag before any open one: not XML this cares about
            return None;
        } else {
            return Some(&rest[1..tag_end(rest)?]);
        }
    }
}

/// Where the start tag beginning at `bytes[0]` ends: the first `>` outside an
/// attribute value.
fn tag_end(bytes: &[u8]) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (at, byte) in bytes.iter().enumerate().skip(1) {
        match (quote, byte) {
            (Some(open), byte) if *byte == open => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(*byte),
            (None, b'>') => return Some(at),
            (None, _) => {}
        }
    }
    None
}

/// One past the `>` that closes a doctype, counting the internal subset's
/// brackets so a `<!ENTITY>` inside it does not end the scan early.
fn doctype_end(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return Some(at + 1),
            _ => {}
        }
    }
    None
}

/// Whether any namespace declaration on the root tag binds EBICS. Attributes
/// are walked rather than searched for as text: `urn:org:ebics:` inside an
/// element's content, or inside an attribute that is not a namespace
/// declaration, says nothing about what the document is.
fn has_ebics_namespace(attributes: &[u8]) -> bool {
    let mut at = 0usize;
    while at < attributes.len() {
        while attributes
            .get(at)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'/')
        {
            at += 1;
        }
        let start = at;
        while attributes
            .get(at)
            .is_some_and(|byte| *byte != b'=' && !byte.is_ascii_whitespace())
        {
            at += 1;
        }
        if at == start {
            return false;
        }
        let name = &attributes[start..at];
        while attributes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if attributes.get(at) != Some(&b'=') {
            // a valueless attribute is not well-formed XML, and it is not a
            // namespace declaration either; take the next one
            continue;
        }
        at += 1;
        while attributes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let Some(quote @ (b'"' | b'\'')) = attributes.get(at).copied() else {
            return false;
        };
        at += 1;
        let value_start = at;
        while attributes.get(at).is_some_and(|byte| *byte != quote) {
            at += 1;
        }
        let value = &attributes[value_start..at];
        at += 1;
        if name.starts_with(b"xmlns") && EBICS_NAMESPACES.iter().any(|ns| value.starts_with(ns)) {
            return true;
        }
    }
    false
}

// ── byte helpers ─────────────────────────────────────────────────────────────

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A case-insensitive substring, for a MIME header whose parameter case is the
/// sender's choice.
fn contains_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Whether `needle` is the first content of the document, past a byte-order
/// mark and any leading blank space. A file that *is* an envelope opens with
/// its armor; the same characters further down belong to something the file
/// contains.
fn document_starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    let body = haystack.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(haystack);
    let at = body
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(body.len());
    body[at..].starts_with(needle)
}

/// The MIME header block, or nothing.
///
/// A media type declared in a header says what the file is; the same characters
/// in a body say what the file talks about. So this is empty unless the first
/// line is a header field at all, and it stops at the blank line that ends the
/// block - without both, a one-line XML document naming a media type in an
/// element was read as an S/MIME envelope.
fn mime_headers(haystack: &[u8]) -> &[u8] {
    let name = haystack
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'-')
        .count();
    if name == 0 || haystack.get(name) != Some(&b':') {
        return &[];
    }
    let end = find(haystack, b"\r\n\r\n")
        .or_else(|| find(haystack, b"\n\n"))
        .unwrap_or(haystack.len());
    &haystack[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tar header block for `name`, checksummed the way a writer does it, so
    /// the fixtures here are valid rather than approximately valid.
    fn tar_header(name: &str, magic: Option<&[u8]>, type_flag: u8) -> Vec<u8> {
        let mut block = vec![0u8; TAR_BLOCK];
        block[..name.len()].copy_from_slice(name.as_bytes());
        block[100..108].copy_from_slice(b"000644 \0"); // mode
        block[108..116].copy_from_slice(b"000000 \0"); // uid
        block[116..124].copy_from_slice(b"000000 \0"); // gid
        block[124..136].copy_from_slice(b"00000001000\0"); // size
        block[136..148].copy_from_slice(b"14657513614 "); // mtime
        block[TAR_TYPE_FLAG] = type_flag;
        if let Some(magic) = magic {
            block[257..257 + magic.len()].copy_from_slice(magic);
        }
        let sum = tar_checksum(&block, false);
        let stored = format!("{sum:06o}\0 ");
        block[TAR_CHECKSUM].copy_from_slice(stored.as_bytes());
        block
    }

    #[test]
    fn container_zip_is_named_from_every_record_signature() {
        for signature in ZIP_SIGNATURES {
            let mut bytes = signature.to_vec();
            bytes.extend_from_slice(b"\x14\x00\x00\x00rest of an archive");
            assert_eq!(detect(&bytes), Some(ContainerKind::Zip), "{signature:?}");
        }
    }

    #[test]
    fn container_tar_is_named_with_ustar_gnu_and_no_magic_at_all() {
        for magic in [Some(b"ustar\0" as &[u8]), Some(b"ustar  \0" as &[u8]), None] {
            let block = tar_header("statement.xml", magic, b'0');
            assert_eq!(detect(&block), Some(ContainerKind::Tar), "{magic:?}");
        }
    }

    #[test]
    fn container_tar_needs_the_checksum_to_agree() {
        let mut block = tar_header("statement.xml", Some(b"ustar\0"), b'0');
        // one byte of the name, so the stored checksum no longer describes it
        block[0] = b'S';
        assert_eq!(detect(&block), None);
    }

    #[test]
    fn container_tar_magic_without_a_checksum_is_not_a_tar() {
        let mut block = vec![b' '; TAR_BLOCK];
        block[..13].copy_from_slice(b"statement.xml");
        block[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect(&block), None);
    }

    #[test]
    fn container_tar_refuses_a_type_flag_no_writer_produces() {
        // `Z` is not a POSIX, pax or GNU type: a checksummed block still is not
        // a tar header if its type says nothing.
        let block = tar_header("statement.xml", Some(b"ustar\0"), b'Z');
        assert_eq!(detect(&block), None);
    }

    #[test]
    fn container_tar_needs_a_whole_header_block() {
        let block = tar_header("statement.xml", Some(b"ustar\0"), b'0');
        assert_eq!(detect(&block[..TAR_BLOCK - 1]), None);
    }

    #[test]
    fn container_pkcs7_is_named_from_armor_smime_and_der() {
        assert_eq!(
            detect(b"-----BEGIN PKCS7-----\nMIIB\n"),
            Some(ContainerKind::Pkcs7)
        );
        assert_eq!(
            detect(b"-----BEGIN CMS-----\nMIIB\n"),
            Some(ContainerKind::Pkcs7)
        );
        assert_eq!(
            detect(b"Content-Type: Application/PKCS7-Mime; smime-type=enveloped-data\r\n\r\n"),
            Some(ContainerKind::Pkcs7)
        );
        // SEQUENCE { OID 1.2.840.113549.1.7.3 (envelopedData), ... }
        let der = b"\x30\x80\x06\x09\x2a\x86\x48\x86\xf7\x0d\x01\x07\x03\xa0\x80";
        assert_eq!(detect(der), Some(ContainerKind::Pkcs7));
    }

    #[test]
    fn container_ordinary_der_is_not_pkcs7() {
        // SEQUENCE { OID 1.2.840.113549.1.1.11 (sha256WithRSA), ... } - the arc
        // next door, and the shape every certificate opens with
        let der = b"\x30\x82\x01\x0a\x06\x09\x2a\x86\x48\x86\xf7\x0d\x01\x01\x0b\x05\x00";
        assert_eq!(detect(der), None);
    }

    #[test]
    fn container_pgp_is_named_from_armor_and_from_packets() {
        assert_eq!(
            detect(b"-----BEGIN PGP MESSAGE-----\n\nhQIMA\n"),
            Some(ContainerKind::Pgp)
        );
        // PKESK v3 of 12 bytes, then a version 1 SEIPD header
        let mut binary = vec![0xC1, 0x0C, 0x03];
        binary.extend_from_slice(&[0u8; 11]);
        binary.extend_from_slice(&[0xD2, 0x20, 0x01]);
        binary.extend_from_slice(&[0u8; 31]);
        assert_eq!(detect(&binary), Some(ContainerKind::Pgp));
        // a SEIPD on its own, version 2
        assert_eq!(
            detect(&[0xD2, 0x10, 0x02, 0x09, 0x01, 0x08, 0x00]),
            Some(ContainerKind::Pgp)
        );
    }

    #[test]
    fn container_a_packet_tag_byte_alone_is_not_pgp() {
        // 0xC3 is a well-formed SKESK header byte; version 0x77 is no SKESK
        // version, and the packet chain stops there.
        assert_eq!(detect(&[0xC3, 0x0D, 0x77, 0x9A, 0x41, 0x00, 0xFF]), None);
        // and a PKESK whose length runs past the prefix cannot be followed
        assert_eq!(detect(&[0xC1, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0x03]), None);
    }

    #[test]
    fn container_text_mentioning_pgp_stays_text() {
        let prose = b"The statement was signed. -----BEGIN PGP MESSAGE----- appears in the \
                      operations runbook, not in this file.\n";
        assert_eq!(detect(prose), None);
    }

    /// A message that carries an envelope is not an envelope. Both armor checks
    /// were unanchored searches over the whole prefix, which refused a statement
    /// shipping its own detached signature and a statement whose free text
    /// happened to name the format.
    #[test]
    fn container_a_message_carrying_armor_is_not_an_envelope() {
        let signed = br#"<?xml version="1.0"?><Document><BkToCstmrStmt/>
 <SplmtryData><Envlp><Sgntr>-----BEGIN PKCS7-----
MIIGxAYJKoZIhvcNAQcC
-----END PKCS7-----</Sgntr></Envlp></SplmtryData></Document>"#;
        assert_eq!(detect(signed), None);

        let narrative = br#"<Document><Ustrd>MIGRATION BEGIN CMS PHASE 2</Ustrd></Document>"#;
        assert_eq!(detect(narrative), None);

        let attachment = br#"<?xml version="1.0"?><Document><SplmtryData><Envlp><Doc>
-----BEGIN PGP MESSAGE-----
hQIMA1234
-----END PGP MESSAGE-----
</Doc></Envlp></SplmtryData></Document>"#;
        assert_eq!(detect(attachment), None);

        let quoted_type = b"<Document><Attchmnt><MmeTp>application/pkcs7-mime</MmeTp>\
                            </Attchmnt></Document>";
        assert_eq!(detect(quoted_type), None);
    }

    /// The file that opens with the armor still is one, byte-order mark and
    /// leading blank line included, and an S/MIME header block is still read.
    #[test]
    fn container_armor_at_the_start_is_still_an_envelope() {
        assert_eq!(
            detect(b"-----BEGIN PKCS7-----\nMIIG\n"),
            Some(ContainerKind::Pkcs7)
        );
        assert_eq!(
            detect("\u{feff}\n  -----BEGIN CMS-----\nMIIG\n".as_bytes()),
            Some(ContainerKind::Pkcs7)
        );
        assert_eq!(
            detect(
                b"MIME-Version: 1.0\r\nContent-Type: application/pkcs7-mime; \
                     smime-type=enveloped-data\r\n\r\nMIIG\r\n"
            ),
            Some(ContainerKind::Pkcs7)
        );
        assert_eq!(
            detect(b"\n-----BEGIN PGP MESSAGE-----\nhQIMA\n"),
            Some(ContainerKind::Pgp)
        );
    }

    /// A tar written by an implementation that summed its header as signed
    /// bytes. Missing this read the archive as one document and merged every
    /// member's rows under one `source_file`.
    #[test]
    fn container_a_signed_char_checksum_is_still_a_tar() {
        let mut block = tar_header("Kontoauszug_M\u{e4}rz.xml", Some(b"ustar\0"), b'0');
        let signed = tar_checksum(&block, true);
        block[TAR_CHECKSUM].copy_from_slice(format!("{signed:06o}\0 ").as_bytes());
        assert!(
            signed != tar_checksum(&block, false),
            "the fixture has to have a byte over 0x7f for the two sums to differ"
        );
        assert_eq!(detect(&block), Some(ContainerKind::Tar));
    }

    /// The first segment of a split set opens with a marker of its own.
    #[test]
    fn container_a_split_archive_marker_is_a_zip() {
        let mut split = b"PK00".to_vec();
        split.extend_from_slice(b"PK\x03\x04\x14\x00");
        assert_eq!(detect(&split), Some(ContainerKind::Zip));
    }

    #[test]
    fn container_ebics_is_named_from_both_namespace_roots() {
        let versioned = br#"<?xml version="1.0"?><ebicsRequest xmlns="urn:org:ebics:H005" Version="H005"><header/></ebicsRequest>"#;
        assert_eq!(detect(versioned), Some(ContainerKind::Ebics));
        let hev = br#"<ebicsHEVResponse xmlns="http://www.ebics.org/H000"><SystemReturnCode/></ebicsHEVResponse>"#;
        assert_eq!(detect(hev), Some(ContainerKind::Ebics));
        let prefixed = br#"<!-- delivery 4 --><!DOCTYPE ebicsRequest [<!ENTITY x "y">]><eb:ebicsRequest xmlns:eb="urn:org:ebics:H004"/>"#;
        assert_eq!(detect(prefixed), Some(ContainerKind::Ebics));
    }

    #[test]
    fn container_ebics_needs_the_name_and_the_namespace_together() {
        // the namespace on a message an EBICS client already unwrapped
        let unwrapped = br#"<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.02" xmlns:eb="urn:org:ebics:H004"><BkToCstmrStmt/></Document>"#;
        assert_eq!(detect(unwrapped), None);
        // the name with no EBICS binding at all
        let named = br#"<ebicsRequest xmlns="urn:example:local"><header/></ebicsRequest>"#;
        assert_eq!(detect(named), None);
        // the namespace as content rather than as a declaration
        let quoted = br#"<Document><Note>urn:org:ebics:H004</Note></Document>"#;
        assert_eq!(detect(quoted), None);
    }

    #[test]
    fn container_an_unfinished_root_tag_is_not_classified() {
        let mut open = br#"<ebicsRequest xmlns="urn:org:ebics:H005" Nm=""#.to_vec();
        open.extend(std::iter::repeat_n(b'x', 70_000));
        assert_eq!(detect(&open), None);
    }

    #[test]
    fn container_ordinary_messages_are_not_containers() {
        let xml = br#"<?xml version="1.0"?><Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.02"><BkToCstmrStmt><GrpHdr><MsgId>X</MsgId></GrpHdr></BkToCstmrStmt></Document>"#;
        assert_eq!(detect(xml), None);
        let mt = b"{1:F01BANKBEBBAXXX0000000000}{2:I103BANKDEFFXXXXN}{4:\n:20:REF\n-}";
        assert_eq!(detect(mt), None);
        // an XML document that quotes the tar magic in a comment
        let ustar =
            br#"<!-- shipped inside a ustar archive --><Document><BkToCstmrStmt/></Document>"#;
        assert_eq!(detect(ustar), None);
        assert_eq!(detect(b""), None);
    }
}
