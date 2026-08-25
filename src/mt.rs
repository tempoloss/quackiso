//! SWIFT MT (ISO 15022 / FIN) framing and field parsing: the MT counterpart of
//! `wire`.
//!
//! MT is not XML. A message is five brace-delimited blocks -
//! `{1:}{2:}{3:}{4:}{5:}` - and block 4 is flat tag-structured text: a field
//! starts at a line spelled `:nn:` or `:nnA:` and its value runs to the next
//! such line. Everything below the parse layer - the row streams, the column
//! writers, the scan driver, gzip - is the same machinery the XML readers use,
//! so only the framing and the leaf shapes live here.
//!
//! Real files come in two shapes and both are read: full block-structured FIN
//! messages, and bare statement files that are block 4 alone (an MT940 dumped by
//! a bank is usually just `:20:` onwards, with no headers at all). The framer
//! settles which shape it is by looking, not by the file name.
//!
//! The reader requires UTF-8. MT is nominally an ASCII character set and real
//! files are not: prowide's `sample_JPchar.txt` carries half-width katakana in a
//! `:86:` and wolph's `with_binary_character.sta` has `Ü` and `ß` in a `:20:`,
//! both valid UTF-8 and both read. A legacy cp852 or latin-1 dump has to be
//! converted first, which is what the error says.
//!
//! Peak memory is one message, capped at [`MAX_MESSAGE_BYTES`] - the text of it,
//! plus one output batch. A statement states its closing balance after its
//! entries and every row carries that balance, so a reader parses the
//! statement-level fields first with [`Fields::without_entries`] and then walks
//! the entries with [`EntryCursor`], one region at a time, holding nothing per
//! entry. That costs three passes over an entry's text and buys a bound that does
//! not follow the entry count: `membound::mt_peak_follows_the_message_text`
//! measures what it costs and `membound::mt_peak_does_not_follow_file_size` what
//! it does not.
//!
//! Nothing message-specific lives here. Each reader keeps its own grain, its own
//! carried context and its own row type.

use std::error::Error;
use std::io::BufRead;
use std::ops::Range;

use crate::decimal;
use crate::temporal;

// ── message framing ──────────────────────────────────────────────────────────

/// The largest single message this will assemble. SWIFT caps a FIN message at
/// 10,000 characters; the only thing that legitimately grows past that is a bare
/// statement body shipped as one message, which reaches this at roughly half a
/// million entries. Past it the file has no message boundary in it and is not a
/// sequence of MT messages.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// One MT message at a time out of a file that may hold many.
///
/// Peak memory is one message, which is the bounded unit MT offers: a statement
/// is one message, the way an `<Ntry>` subtree is the bounded unit of camt.053.
pub struct MtReader<R: BufRead> {
    reader: R,
    source: String,
    /// The tail of a line that turned out to begin the next message.
    pending: Option<String>,
    /// The ceiling on one assembled message.
    limit: usize,
    /// Lines read out of `reader` so far. A line served from `pending` does not
    /// advance it: it was counted when it was read.
    taken: usize,
    /// The file line the message under construction began at.
    line: usize,
}

impl<R: BufRead> MtReader<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Self::with_limit(reader, source, MAX_MESSAGE_BYTES)
    }

    /// The same with the ceiling stated: how a test proves the cap without
    /// allocating [`MAX_MESSAGE_BYTES`].
    pub fn with_limit(reader: R, source: &str, limit: usize) -> Self {
        MtReader {
            reader,
            source: source.to_string(),
            pending: None,
            limit,
            taken: 0,
            line: 1,
        }
    }

    /// The file line the message last returned began at.
    pub fn line(&self) -> usize {
        self.line
    }

    /// The next message's text, or `None` at end of input.
    pub fn next_message(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        let mut msg = String::new();
        // Which shape this message is turning out to be. Block 1 already set is
        // what makes a further `{1:` a new message rather than a stray brace;
        // a `:20:` already seen is the same guard for a bare statement file.
        let mut has_block1 = false;
        let mut has_field20 = false;

        loop {
            let Some(line) = self.take_line()? else {
                return Ok(finished(msg));
            };
            let line = line.trim_end_matches(['\n', '\r']);
            let trimmed = line_body(line);

            // RJE separator: ends the current message and belongs to neither.
            if trimmed == "$" {
                if !msg.trim().is_empty() {
                    return Ok(Some(msg));
                }
                continue;
            }

            // Where the next message begins in this line, if it does. A `{1:`
            // is only a boundary once block 1 of the current message is set --
            // and a single line may hold whole messages glued together, so the
            // search starts past this message's own block 1 rather than at 0.
            let boundary = match has_block1 {
                true => line.find("{1:"),
                false => line
                    .find("{1:")
                    .and_then(|own| line[own + 3..].find("{1:").map(|at| own + 3 + at)),
            };
            if let Some(at) = boundary {
                if at > 0 {
                    if msg.is_empty() {
                        self.line = self.taken;
                    }
                    msg.push_str(&line[..at]);
                    msg.push('\n');
                }
                self.pending = Some(line[at..].to_string());
                return Ok(Some(msg));
            }

            if !has_block1 && has_field20 && line.starts_with(":20:") {
                self.pending = Some(line.to_string());
                return Ok(Some(msg));
            }
            if msg.is_empty() && !line.contains("{1:") && !line.starts_with(":20:") {
                // Whatever sits before the first statement of a bare file
                // belongs to no message.
                continue;
            }

            // A statement with no blocks ends at its own terminator; block 4
            // ends at `-}`, which is not this line and must not cut block 5 off.
            if !has_block1 && trimmed == "-" {
                if !msg.trim().is_empty() {
                    return Ok(Some(msg));
                }
                continue;
            }

            has_block1 |= line.contains("{1:");
            has_field20 |= line.starts_with(":20:");
            if msg.len() + line.len() > self.limit {
                return Err(format!(
                    "{}: no MT message boundary in the first {} bytes; is this a SWIFT MT file?",
                    self.source, self.limit
                )
                .into());
            }
            if msg.is_empty() {
                self.line = self.taken;
            }
            msg.push_str(line);
            msg.push('\n');
        }
    }

    fn take_line(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        if let Some(held) = self.pending.take() {
            return Ok(Some(held));
        }
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => {
                self.taken += 1;
                Ok(Some(line))
            }
            // The reader needs UTF-8: real MT carries non-ASCII in a narrative
            // and in a reference, and that is read. A legacy cp852 or latin-1
            // dump has to be converted before it can be, which is what this
            // says by naming the path and the byte offset it stopped at.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                Err(format!("{}: not UTF-8 text: {e}", self.source).into())
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn finished(msg: String) -> Option<String> {
    (!msg.trim().is_empty()).then_some(msg)
}

/// A line without the transmission's own characters around it. `mBank`'s
/// statements end their block 4 with `-` and an ETX, which belongs to the wire
/// and not to the field above it; a plain `trim` leaves the ETX behind and the
/// terminator goes unrecognised.
fn line_body(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || c.is_control())
}

// ── blocks and the headers in them ───────────────────────────────────────────

/// The content of block `n`, without its `{n:` and closing delimiter.
pub fn block(msg: &str, n: u8) -> Option<&str> {
    block_at(msg, n).map(|(content, _)| content)
}

/// The byte range of block `n`'s content within `msg`, and the 1-based line that
/// content starts on. A field's position in the file is the message's line plus
/// this plus the field's own.
///
/// Blocks 3 and 5 nest `{tag:value}` pairs, so the close is found by brace
/// depth rather than by the next `}`. Block 4 is the exception: it ends at `-}`,
/// and its content is text that may itself contain braces.
pub fn block_span(msg: &str, n: u8) -> Option<(Range<usize>, usize)> {
    let pattern = [b'{', b'0' + n, b':'];
    let bytes = msg.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if depth == 0 && bytes[i..].starts_with(&pattern) {
            let start = i + 3;
            return if n == 4 {
                let end = msg[start..]
                    .find("-}")
                    .map(|at| start + at)
                    .unwrap_or(msg.len());
                let raw = &msg[start..end];
                let led = raw.len() - raw.trim_start_matches(['\n', '\r']).len();
                let kept = raw.trim_matches(['\n', '\r']).len();
                let span = start + led..start + led + kept;
                let line = line_of(msg, span.start);
                Some((span, line))
            } else {
                close_brace(msg, start).map(|end| (start..end, line_of(msg, start)))
            };
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The same, resolved to the slice it names.
pub fn block_at(msg: &str, n: u8) -> Option<(&str, usize)> {
    block_span(msg, n).map(|(span, line)| (&msg[span], line))
}

/// The 1-based line byte `offset` sits on.
fn line_of(msg: &str, offset: usize) -> usize {
    msg[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

/// Where a field sits in the file: the message's first line, plus the body's
/// offset inside the message, plus the field's offset inside the body.
pub fn at(message_line: usize, body_line: usize, field_line: usize) -> usize {
    message_line + body_line + field_line - 2
}

/// The `}` that closes a block whose content starts at `from`.
fn close_brace(msg: &str, from: usize) -> Option<usize> {
    let mut depth = 1usize;
    for (i, b) in msg.bytes().enumerate().skip(from) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `I` for a message sent to the network, `O` for one delivered from it. Block 2
/// says so in its first character; a block 2 that does not is dated by its
/// length, because an input header cannot reach the 28-character MIR an output
/// header carries.
pub fn direction(msg: &str) -> Option<&'static str> {
    let b2 = block(msg, 2)?;
    Some(match b2.as_bytes().first()? {
        b'I' => "I",
        b'O' => "O",
        _ if b2.len() <= 23 => "I",
        _ => "O",
    })
}

/// The three-digit MT number: `103`, `202`, `940`, `942`.
pub fn message_type(msg: &str) -> Option<&str> {
    let b2 = block(msg, 2)?;
    let start = match b2.as_bytes().first()? {
        b'I' | b'O' => 1,
        _ => 0,
    };
    let mt = b2.get(start..start + 3)?;
    mt.bytes().all(|b| b.is_ascii_digit()).then_some(mt)
}

/// Who sent it. An input message names its sender in block 1; an output message
/// names it in the MIR inside block 2, because block 1 then holds the network's
/// own address.
pub fn sender_bic(msg: &str) -> Option<String> {
    match direction(msg)? {
        "I" => bic11(logical_terminal(msg)?),
        _ => bic11(block(msg, 2)?.get(14..26)?),
    }
}

/// Who receives it: the mirror of [`sender_bic`].
pub fn receiver_bic(msg: &str) -> Option<String> {
    match direction(msg)? {
        "I" => bic11(block(msg, 2)?.get(4..16)?),
        _ => bic11(logical_terminal(msg)?),
    }
}

/// The 12-character logical terminal address in block 1, after the application
/// and service identifiers.
fn logical_terminal(msg: &str) -> Option<&str> {
    block(msg, 1)?.get(3..15)
}

/// A logical terminal address as the BIC it contains. The 9th character is the
/// terminal identifier, not part of the BIC, and dropping it is what makes an MT
/// party join against a `BICFI` out of an ISO 20022 reader.
fn bic11(lt: &str) -> Option<String> {
    (lt.len() == 12 && lt.is_ascii()).then(|| format!("{}{}", &lt[..8], &lt[9..12]))
}

/// A `{tag:value}` field of the user header: `121` the UETR, `119` the
/// validation flag (STP, COV, REMIT), `108` the sender's reference.
pub fn user_header_field<'a>(msg: &'a str, tag: &str) -> Option<&'a str> {
    let b3 = block(msg, 3)?;
    let open = format!("{{{tag}:");
    let at = b3.find(&open)? + open.len();
    let end = b3[at..].find('}')? + at;
    let value = b3[at..end].trim();
    (!value.is_empty()).then_some(value)
}

/// Whether this message is one a reader for `mt` should read.
///
/// Block 2 names the type when the message has one. With no block 2 there are two
/// cases and block 1 separates them: a bare block-4 statement body, which is how
/// banks ship MT940, and a service envelope such as an ACK, which carries block 1
/// and no block 2 and is not a message at all. A bare body is claimed only by the
/// reader whose own mandatory field it carries: 23B for MT103, 58a for MT202, 60a
/// for MT940, 34F for MT942, which no other of the four has.
pub fn claims(msg: &str, fields: &Fields<'_>, mt: &str) -> bool {
    match message_type(msg) {
        Some(found) => found == mt,
        None if block(msg, 1).is_some() => false,
        None => match mt {
            "103" => fields.find("23B").is_some(),
            "202" => fields.find("58").is_some(),
            "940" => fields.find("60").is_some(),
            "942" => fields.find("34F").is_some(),
            _ => false,
        },
    }
}

// ── the field tokenizer ──────────────────────────────────────────────────────

/// One entry of a statement, as the region of the body it occupies.
///
/// The `:61:` line, its continuation and the `:86:` fields under it, as a byte
/// range into the body plus the 1-based line the `:61:` sits on. One of these is
/// alive at a time: [`EntryCursor`] finds the next region when the reader asks
/// for it, so a statement costs its text and one output batch.
pub struct EntrySite {
    pub bytes: Range<usize>,
    pub line: usize,
}

/// How far the entry walk has got: the byte offset of the next line to read, how
/// many lines it has read, and the region it has opened but not yet closed.
///
/// Not an iterator, because the body it walks is owned by the reader that holds
/// the cursor, and an iterator would have to borrow it. None of the three fields
/// grows with the statement, and the cursor reads each line of the body exactly
/// once across all its calls, which is what keeps a statement of half a million
/// entries bounded by its own text.
#[derive(Default)]
pub struct EntryCursor {
    at: usize,
    seen: usize,
    /// The region opened but not yet closed: where it starts, and the line it
    /// starts on. It survives the return that hands back the region before it, so
    /// the line that opened it is never read a second time.
    open: Option<(usize, usize)>,
}

impl EntryCursor {
    /// The next entry region at or after the cursor, or `None` once the body is
    /// spent. `entry` is matched the way [`Fields::find`] matches, so `"61"`
    /// finds `:61:` whatever option letter follows the number.
    ///
    /// `body` must be the same slice on every call: the cursor holds offsets into
    /// it, not a borrow of it.
    pub fn next_site(&mut self, body: &str, entry: &str) -> Option<EntrySite> {
        loop {
            if self.at > body.len() {
                return self.open.take().map(|(from, first)| EntrySite {
                    bytes: from..body.len(),
                    line: first,
                });
            }
            let raw = body[self.at..].split('\n').next().unwrap_or("");
            let at = self.at;
            self.at += raw.len() + 1;
            self.seen += 1;
            let index = self.seen;
            let line = raw.trim_end_matches('\r');
            let trimmed = line_body(line);
            match classify(line, trimmed, entry, self.open.is_some()) {
                // A region open at the terminator ends before it, and the body is
                // spent: the same rule `Fields::without_entries` stops on.
                Line::Terminator => {
                    self.at = body.len() + 1;
                    return self.open.take().map(|(from, first)| EntrySite {
                        bytes: from..at,
                        line: first,
                    });
                }
                Line::Entry => {
                    if let Some((from, first)) = self.open.replace((at, index)) {
                        return Some(EntrySite {
                            bytes: from..at,
                            line: first,
                        });
                    }
                }
                Line::Field(..) => {
                    if let Some((from, first)) = self.open.take() {
                        return Some(EntrySite {
                            bytes: from..at,
                            line: first,
                        });
                    }
                }
                Line::Narrative | Line::Blank | Line::Continuation => {}
            }
        }
    }
}

/// One field of a message body.
pub struct Field<'a> {
    pub tag: &'a str,
    pub value: String,
    /// 1-based line of this field's first line within the body it was parsed from.
    pub line: usize,
}

/// The fields of a message body, in the order they were written.
///
/// Tags are borrowed from the message; values are rebuilt because a multi-line
/// value (`:50K:`, `:59:`, `:70:`, `:72:`, `:77B:`, and the second line of `:61:`)
/// is joined with a single `\n` whatever the file's line endings were.
///
/// `:86:` is the exception, and it is a format difference rather than a special
/// case: 86 is `6*65x`, one logical value wrapped at a fixed width, so its
/// continuation lines are appended raw and untrimmed. The `n*35x` fields above
/// break where the writer meant them to, and there the newline is the content.
pub struct Fields<'a>(Vec<Field<'a>>);

impl<'a> Fields<'a> {
    /// Tokenize a message body: block 4, or the whole message when the file is
    /// a bare statement with no blocks at all.
    pub fn parse(body: &'a str) -> Self {
        Self::without_entries(body, "").0
    }

    /// The statement-level fields of a body, and how many entries sit between
    /// them.
    ///
    /// `entry` is the tag an entry starts with, matched the way [`Fields::find`]
    /// matches, so `"61"` finds `:61:` whatever letter might follow the number.
    /// An entry runs from its own line to the line before the next field that is
    /// neither a continuation nor an `:86:`: the narrative a bank writes under an
    /// entry stays with the entry, and the one it writes under the closing
    /// balance does not. The fields inside those regions are NOT in the returned
    /// `Fields` - they are what [`EntryCursor`] hands back one region at a time.
    /// The count is kept because a caller has to know whether the statement has
    /// entries at all before it can emit its first row.
    pub fn without_entries(body: &'a str, entry: &str) -> (Self, usize) {
        let mut out: Vec<Field<'a>> = Vec::new();
        let mut entries = 0usize;
        // True while the walk is inside an entry region.
        let mut inside = false;

        for (index, raw) in body.split('\n').enumerate() {
            let line = raw.trim_end_matches('\r');
            let trimmed = line_body(line);
            match classify(line, trimmed, entry, inside) {
                Line::Terminator => break,
                Line::Entry => {
                    inside = true;
                    entries += 1;
                }
                Line::Narrative => {}
                Line::Field(tag, value) => {
                    inside = false;
                    out.push(Field {
                        tag,
                        value: match tag {
                            "86" => value.to_string(),
                            _ => value.trim().to_string(),
                        },
                        line: index + 1,
                    });
                }
                Line::Blank => {}
                // A continuation inside an entry belongs to the entry's region;
                // outside one it extends the field above it.
                Line::Continuation if inside => {}
                Line::Continuation => {
                    if let Some(field) = out.last_mut() {
                        if field.tag == "86" {
                            field.value.push_str(line);
                        } else {
                            field.value.push('\n');
                            field.value.push_str(trimmed);
                        }
                    }
                }
            }
        }
        (Fields(out), entries)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The first field matching `key`. A two-character `key` is the field number
    /// and matches whichever option letter the message chose (`"50"` finds `50A`,
    /// `50F` or `50K`); a longer `key` is the exact tag.
    pub fn find(&self, key: &str) -> Option<&Field<'a>> {
        self.find_in(0..self.0.len(), key)
    }

    /// The value of the first field matching `key`.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.find(key).map(|field| field.value.as_str())
    }

    /// Every value matching `key`, in order. Repeated tags (`:13C:`, `:23E:`,
    /// `:71F:`, `:86:`) are each yielded.
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.all_in(0..self.0.len(), key)
    }

    pub fn find_in(&self, range: Range<usize>, key: &str) -> Option<&Field<'a>> {
        self.0
            .get(range)?
            .iter()
            .find(|field| matches_key(field.tag, key))
    }

    pub fn all_in(&self, range: Range<usize>, key: &str) -> Vec<&str> {
        self.0
            .get(range)
            .unwrap_or_default()
            .iter()
            .filter(|field| matches_key(field.tag, key))
            .map(|field| field.value.as_str())
            .collect()
    }

    /// Where the first of `keys` sits. A sequence boundary is a position, not a
    /// tag: `:52a:` and `:72:` occur in both sequences of an MT202COV, so the
    /// reader that wants sequence B asks where it starts and looks from there.
    pub fn position(&self, keys: &[&str]) -> Option<usize> {
        self.0
            .iter()
            .position(|field| keys.iter().any(|key| matches_key(field.tag, key)))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Field<'a>> + '_ {
        self.0.iter()
    }
}

fn matches_key(tag: &str, key: &str) -> bool {
    if key.len() == 2 {
        tag.starts_with(key)
    } else {
        tag == key
    }
}

/// A field start, and only a field start: two digits then `:`, or two digits and
/// one capital then `:`. Nothing else counts, which is why a `:` inside a
/// `:77B:` or `:86:` value cannot be mistaken for one.
fn split_field(line: &str) -> Option<(&str, &str)> {
    let b = line.as_bytes();
    if b.first() != Some(&b':') || b.len() < 4 {
        return None;
    }
    if !(b[1].is_ascii_digit() && b[2].is_ascii_digit()) {
        return None;
    }
    let end = if b[3] == b':' {
        3
    } else if b.len() >= 5 && b[3].is_ascii_uppercase() && b[4] == b':' {
        4
    } else {
        return None;
    };
    Some((&line[1..end], &line[end + 1..]))
}

/// What a line is to a body walk.
///
/// Both walks over a statement body classify a line the same way and differ only
/// in what they keep: [`Fields::without_entries`] builds the statement-level
/// fields, [`EntryCursor::next_site`] finds the region boundaries. The rule lives
/// here once because it is subtle in three places at once, and two copies of it
/// drifted apart is the failure this shape prevents.
///
/// The regions stay byte ranges rather than parsed `Fields`: folding the
/// tokenizer into the cursor would put the continuation-joining rule in two
/// places, which is the thing this enum exists to stop.
enum Line<'a> {
    /// The terminator. The body ends here, and a region open at it ends before it.
    Terminator,
    /// A line that starts an entry.
    Entry,
    /// An `:86:` under an open entry region: it belongs to the entry and not to
    /// the statement.
    Narrative,
    /// A field of the statement, which closes any region open above it.
    Field(&'a str, &'a str),
    /// An empty line. It falls inside an open region and extends nothing outside
    /// one.
    Blank,
    /// A continuation. It falls inside an open region and extends the field above
    /// it outside one.
    Continuation,
}

/// Classify one line of a body. `line` has had its `\r` taken off, `trimmed` is
/// [`line_body`] of it, and `inside` says whether an entry region is open, which
/// is the only thing that makes an `:86:` a narrative rather than a field.
fn classify<'a>(line: &'a str, trimmed: &str, entry: &str, inside: bool) -> Line<'a> {
    // The terminator may carry junk behind it: a stray brace after `-}` in a file
    // committed that way, or the ETX that closes a real transmission, which
    // `line_body` has already taken off.
    if trimmed.starts_with("-}") || trimmed == "-" {
        return Line::Terminator;
    }
    match split_field(line) {
        Some((tag, _)) if matches_key(tag, entry) => Line::Entry,
        Some((tag, _)) if tag == "86" && inside => Line::Narrative,
        Some((tag, value)) => Line::Field(tag, value),
        None if trimmed.is_empty() => Line::Blank,
        None => Line::Continuation,
    }
}

// ── leaf parsers ─────────────────────────────────────────────────────────────

/// The three columns an absent `:32A:` leaves NULL together: a value date, its
/// currency and its amount all come out of one field, so a reader that has the
/// field has all three and a reader that lacks it has none.
pub type DateCcyAmount = (Option<i32>, Option<String>, Option<i128>);

/// The same for `:90D:`/`:90C:`: how many entries, in which currency, totalling
/// what.
pub type CountCcyAmount = (Option<i64>, Option<String>, Option<i128>);

/// The `:34F:` floor limits: the debit side's amount and currency, then the
/// credit side's. One occurrence with no side marked fills both.
pub type FloorLimits = (Option<i128>, Option<String>, Option<i128>, Option<String>);

/// Two-digit years below this belong to the 2000s, the rest to the 1900s. Fixed
/// rather than sliding: a window that moves with the clock would make the same
/// file parse differently next year, and 1990s archives are still read.
pub const YEAR_PIVOT: u32 = 69;

/// An MT amount into an integer scaled by `10^decimal::SCALE`.
///
/// The decimal separator is a comma and may be the last character (`9,` is
/// nine). `decimal::scaled` rejects a comma outright, so the swap is not
/// cosmetic. A malformed amount is an error, never a NULL: a NULL vanishes from
/// a `SUM` and hands back a plausible wrong total.
pub fn amount(text: &str) -> Result<i128, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("empty amount".into());
    }
    let mut commas = 0;
    for b in s.bytes() {
        match b {
            b',' => commas += 1,
            b'0'..=b'9' => {}
            _ => return Err(format!("not an amount: {text:?}")),
        }
    }
    if commas > 1 {
        return Err(format!("not an amount: {text:?}"));
    }
    let dotted = s.replace(',', ".");
    decimal::scaled(dotted.trim_end_matches('.'))
}

/// `YYMMDD` as a DuckDB DATE, and the century the pivot resolved it to.
///
/// An unreadable date is `None`, the rule `temporal` states for every other
/// reader: a date that cannot be read is missing, while a malformed amount stays
/// an error because a missing amount silently changes a `SUM`.
fn date2_full(yymmdd: &str) -> Option<(i32, i32)> {
    let s = yymmdd.trim();
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let yy: u32 = s[..2].parse().ok()?;
    let year = if yy < YEAR_PIVOT {
        2000 + yy as i32
    } else {
        1900 + yy as i32
    };
    let days = temporal::date_days(&format!("{year:04}{}", &s[2..]))?;
    Some((days, year))
}

/// `YYMMDD` as a DuckDB DATE.
pub fn date2(yymmdd: &str) -> Option<i32> {
    date2_full(yymmdd).map(|(days, _)| days)
}

/// The `MMDD` entry date of a statement line, given the value date it sits
/// beside.
///
/// The year is not on the wire. Taking the value date's year is right except
/// across a year boundary, where a booking in late December and a value date in
/// early January are eleven months apart on paper and two days apart in fact -
/// so a gap of 330 days or more means the entry belongs to the neighbouring
/// year, not to this one.
pub fn entry_date(mmdd: &str, value_days: i32, value_year: i32) -> Option<i32> {
    let s = mmdd.trim();
    if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let candidate = |year: i32| temporal::date_days(&format!("{year:04}{s}"));
    // 29 February resolves only in the leap year it belongs to, which may be
    // the neighbour rather than the value date's own.
    let mut days = candidate(value_year)
        .or_else(|| candidate(value_year - 1))
        .or_else(|| candidate(value_year + 1))?;
    if (days - value_days).abs() >= 330 {
        let neighbour = if days > value_days {
            value_year - 1
        } else {
            value_year + 1
        };
        if let Some(shifted) = candidate(neighbour) {
            days = shifted;
        }
    }
    Some(days)
}

/// `:13D:` - `YYMMDDHHMMsHHMM`. The timestamp is returned as written, with the
/// offset beside it rather than folded into it: the report time a bank states is
/// its own local time, and rewriting it to UTC loses which day the bank meant.
pub fn datetime13d(v: &str) -> Result<(Option<i64>, String), String> {
    let s = v.trim();
    if s.len() < 15 || !s.is_ascii() {
        return Err(format!("not a date, time and offset: {v:?}"));
    }
    let micros = date2_full(&s[..6]).and_then(|(_, year)| {
        let stamp = format!(
            "{year:04}-{}-{}T{}:{}:00",
            &s[2..4],
            &s[4..6],
            &s[6..8],
            &s[8..10]
        );
        temporal::ts_micros(&stamp)
    });
    let sign = &s[10..11];
    if sign != "+" && sign != "-" {
        return Err(format!("not a UTC offset: {v:?}"));
    }
    if !s[11..15].bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("not a UTC offset: {v:?}"));
    }
    Ok((micros, format!("{sign}{}", &s[11..15])))
}

/// A balance field: `:60F:`, `:60M:`, `:62F:`, `:62M:`, `:64:`, `:65:`.
pub struct Balance {
    pub dc: String,
    pub date: Option<i32>,
    pub currency: String,
    pub amount: i128,
}

pub fn balance(tag: &str, v: &str) -> Result<Balance, String> {
    let s = v.trim();
    if !s.is_ascii() || s.len() < 11 {
        return Err(format!(":{tag}: not a balance: {v:?}"));
    }
    let dc = &s[..1];
    if dc != "C" && dc != "D" {
        return Err(format!(":{tag}: not a credit or debit mark: {dc:?}"));
    }
    Ok(Balance {
        dc: dc.to_string(),
        date: date2(&s[1..7]),
        currency: s[7..10].to_string(),
        amount: amount(&s[10..]).map_err(|e| format!(":{tag}: {e}"))?,
    })
}

/// `:32A:` - value date, currency, amount.
pub fn date_ccy_amount(v: &str) -> Result<(Option<i32>, String, i128), String> {
    let s = v.trim();
    if !s.is_ascii() || s.len() < 10 {
        return Err(format!("not a date, currency and amount: {v:?}"));
    }
    let currency = &s[6..9];
    if !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(format!("not a currency: {currency:?}"));
    }
    Ok((date2(&s[..6]), currency.to_string(), amount(&s[9..])?))
}

/// `:33B:`, `:71F:`, `:71G:` - currency and amount.
pub fn ccy_amount(tag: &str, v: &str) -> Result<(String, i128), String> {
    let s = v.trim();
    if !s.is_ascii() || s.len() < 4 {
        return Err(format!(":{tag}: not a currency and amount: {v:?}"));
    }
    let currency = &s[..3];
    if !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(format!(":{tag}: not a currency: {currency:?}"));
    }
    Ok((
        currency.to_string(),
        amount(&s[3..]).map_err(|e| format!(":{tag}: {e}"))?,
    ))
}

/// `:90D:`, `:90C:` - how many entries, and their total.
pub fn count_ccy_amount(v: &str) -> Result<(Option<i64>, String, i128), String> {
    let s = v.trim();
    if !s.is_ascii() {
        return Err(format!("not a count, currency and amount: {v:?}"));
    }
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    let (count, rest) = (s[..digits].parse().ok(), &s[digits..]);
    let (currency, amount) = ccy_amount("90", rest)?;
    Ok((count, currency, amount))
}

/// `:28C:` - statement number, and the page of it this message is.
pub fn statement_number(v: &str) -> (Option<i64>, Option<i64>) {
    let s = v.trim();
    let (number, sequence) = match s.split_once('/') {
        Some((number, sequence)) => (number, Some(sequence)),
        None => (s, None),
    };
    (
        number.trim().parse().ok(),
        sequence.and_then(|s| s.trim().parse().ok()),
    )
}

/// One `:61:` statement line: the entry itself.
pub struct StatementLine {
    pub value_date: Option<i32>,
    pub entry_date: Option<i32>,
    pub credit_debit: String,
    pub funds_code: Option<String>,
    pub amount: i128,
    pub transaction_type: Option<String>,
    pub transaction_code: Option<String>,
    pub customer_ref: Option<String>,
    pub bank_ref: Option<String>,
    pub supplementary: Option<String>,
}

/// Slice a `:61:` line into its ten subfields.
///
/// The format has no separators, so the boundaries are found by what the
/// characters are: a run of digits and commas is a date or an amount, a run of
/// neither is the credit/debit mark. Two details are not guessable and both are
/// taken from Prowide's own slicer: an `R` or `E` opening the mark makes it two
/// characters (a reversal or an expected entry), and the references split on the
/// FIRST `//`, so a single `/` stays inside the customer reference.
pub fn statement_line(v: &str) -> Result<StatementLine, String> {
    let mut parts = v.splitn(2, '\n');
    let head = parts.next().unwrap_or("").trim();
    let supplementary = parts
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if !head.is_ascii() {
        return Err(format!("not a statement line: {v:?}"));
    }
    let b = head.as_bytes();
    let numeric = |i: usize| b[i].is_ascii_digit() || b[i] == b',';

    // (1) the dates. Bounded to ten characters: a comma counts as numeric here,
    // so an unbounded run would eat the amount of a malformed line.
    let mut i = 0;
    while i < b.len() && i < 10 && numeric(i) {
        i += 1;
    }
    if i < 6 {
        return Err(format!("statement line has no value date: {v:?}"));
    }
    let dates = date2_full(&head[..6]);
    // With no year the 330-day correction has nothing to work from, so an
    // unreadable value date takes the entry date with it.
    let entry_date = match (i >= 10, dates) {
        (true, Some((value_date, value_year))) => entry_date(&head[6..10], value_date, value_year),
        _ => None,
    };

    // Some banks pad an absent entry date with spaces instead of omitting it.
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }

    // (2) the credit/debit mark, and a funds code if one follows it.
    let mark_at = i;
    while i < b.len() && !numeric(i) {
        i += 1;
    }
    let mark = &head[mark_at..i];
    let mark_len = if mark.starts_with('R') || mark.starts_with('E') {
        2
    } else {
        1
    };
    if mark.len() < mark_len {
        return Err(format!("statement line has no credit or debit mark: {v:?}"));
    }
    let credit_debit = mark[..mark_len].to_string();
    let funds_code = mark
        .get(mark_len..mark_len + 1)
        .map(|code| code.to_string());

    // (3) the amount.
    let amount_at = i;
    while i < b.len() && numeric(i) {
        i += 1;
    }
    if i == amount_at {
        return Err(format!("statement line has no amount: {v:?}"));
    }
    let amount = amount(&head[amount_at..i])?;

    // (4) transaction type and code, then (5) the two references.
    let rest = &head[i..];
    let (transaction_type, transaction_code, refs) = if rest.len() >= 4 {
        (
            Some(rest[..1].to_string()),
            Some(rest[1..4].to_string()),
            &rest[4..],
        )
    } else {
        (None, None, "")
    };
    let (customer_ref, bank_ref) = match refs.find("//") {
        Some(at) => (some_text(&refs[..at]), some_text(&refs[at + 2..])),
        None => (some_text(refs), None),
    };

    Ok(StatementLine {
        value_date: dates.map(|(days, _)| days),
        entry_date,
        credit_debit,
        funds_code,
        amount,
        transaction_type,
        transaction_code,
        customer_ref,
        bank_ref,
        supplementary,
    })
}

fn some_text(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Which MT number a message is, from block 2, or -- for the bodies banks ship
/// with no headers at all -- from the mandatory field only one type carries.
/// Statement types first: those are the ones that arrive bare.
pub fn message_number(msg: &str, fields: &Fields<'_>) -> Option<String> {
    message_type(msg).map(str::to_string).or_else(|| {
        ["940", "942", "103", "202"]
            .into_iter()
            .find(|number| claims(msg, fields, number))
            .map(str::to_string)
    })
}

/// What the identifier line of a party field is. The option letter decides it,
/// but not on its own: a `C` on field 50 is the BIC of whoever instructed the
/// payment, and a `C` on 52 or 57 is an account number and nothing else. An `A`
/// names a BIC anywhere, a `B` names a location inside the sender's own
/// institution, an `L` names an instructing party as free text.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Identifies {
    Bic,
    Name,
    Location,
    Nothing,
}

pub fn identifies(tag: &str) -> Identifies {
    match (tag.get(..2), tag.as_bytes().get(2)) {
        (_, Some(b'A' | b'G')) => Identifies::Bic,
        (Some("50"), Some(b'C')) => Identifies::Bic,
        (_, Some(b'C')) => Identifies::Nothing,
        (_, Some(b'B')) => Identifies::Location,
        _ => Identifies::Name,
    }
}

/// Every part of a party field, laid out the way its option letter says.
///
/// The readers want the identifier and the account. The address audit wants what
/// the readers drop: the lines after the name, and -- in option F -- the town and
/// country the format states in a subfield of their own. Both come from here
/// rather than from two parsers, because two parsers of one field drift.
#[derive(Debug, Default, Clone)]
pub struct PartyField<'a> {
    pub account: Option<&'a str>,
    /// `/C/` or `/D/` on the account line, for the options that carry a side.
    pub mark: Option<&'a str>,
    /// The line the option's second subfield begins with: a BIC, a name or a
    /// location. [`identifies`] says which.
    pub identifier: Option<&'a str>,
    /// Address lines, the name excluded: the free-text lines of D, H, K and the
    /// letterless 59, or the `2/` subfields of F.
    pub lines: Vec<&'a str>,
    /// From `3/BE/BRUSSELS` in option F, and nowhere else. No other option
    /// states either separately, which is the whole reason F exists.
    pub town: Option<&'a str>,
    pub country: Option<&'a str>,
}

impl PartyField<'_> {
    /// How many address elements the field states in one of its own subfields,
    /// counted the way `audit_addresses` counts `<TwnNm>` and `<Ctry>`.
    pub fn structured(&self) -> i64 {
        i64::from(self.town.is_some()) + i64::from(self.country.is_some())
    }
}

/// Turns a field into the `path:line` prefix its errors carry.
pub type At<'a> = dyn Fn(&Field<'_>) -> String + 'a;

/// Where each repetition of a sequence sits in the field list, split on the exact
/// tag that opens one. The header is everything before the first.
///
/// The tag has to be exact. A two-character key matches whatever option letter
/// follows it, and the messages that repeat a sequence carry near-namesakes of
/// their own boundary: an MT101 has `:21R:` in its header and `:21F:` in a
/// transaction, an MT104 has `:21R:` and `:21C:`, and matching loosely would open
/// a sequence on any of them.
pub fn sequences(fields: &Fields<'_>, tag: &str) -> Vec<Range<usize>> {
    let starts: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.tag == tag)
        .map(|(index, _)| index)
        .collect();
    let end = fields.len();
    starts
        .iter()
        .enumerate()
        .map(|(n, from)| *from..starts.get(n + 1).copied().unwrap_or(end))
        .collect()
}

/// `:28D:` is a message's place in a series: `1/3` is the first of three, and a
/// bank splitting a large batch sends them numbered.
pub fn index_total(value: &str) -> (Option<i64>, Option<i64>) {
    let (index, total) = match value.trim().split_once('/') {
        Some(pair) => pair,
        None => (value.trim(), ""),
    };
    (index.parse().ok(), total.parse().ok())
}

/// The party instructing the bank: options C and L of field 50a, which carry an
/// identifier and no address. The same two options in every message type that has
/// an instructing party, which is why the number alone is not enough.
pub fn instructing_party(fields: &Fields<'_>, span: Range<usize>) -> Option<String> {
    ["50C", "50L"]
        .into_iter()
        .find_map(|tag| fields.find_in(span.clone(), tag))
        .and_then(|field| party(field.tag, &field.value).0)
}

/// The customer named by one of `tags`, as (option letter, identifier, account).
///
/// Field 50a is two fields sharing a number and the option letter is the only
/// thing that tells them apart, so the caller states which options it means: an
/// MT101 debits `50F`, `50G` or `50H`, and an MT104 collects for `50A` or `50K`.
pub fn customer(
    fields: &Fields<'_>,
    span: Range<usize>,
    tags: &[&str],
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(field) = tags
        .iter()
        .find_map(|tag| fields.find_in(span.clone(), tag))
    else {
        return (None, None, None);
    };
    let (identifier, account, _) = party(field.tag, &field.value);
    (option_letter(field.tag), identifier, account)
}

/// The party matching `key` in `span`, as (option letter, identifier, account).
pub fn party_with_option(
    fields: &Fields<'_>,
    span: Range<usize>,
    key: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(field) = fields.find_in(span, key) else {
        return (None, None, None);
    };
    let (identifier, account, _) = party(field.tag, &field.value);
    (option_letter(field.tag), identifier, account)
}

/// The party matching `key` in `span`, as (identifier, account).
pub fn party_in(
    fields: &Fields<'_>,
    span: Range<usize>,
    key: &str,
) -> (Option<String>, Option<String>) {
    let Some(field) = fields.find_in(span, key) else {
        return (None, None);
    };
    let (identifier, account, _) = party(field.tag, &field.value);
    (identifier, account)
}

fn option_letter(tag: &str) -> Option<String> {
    tag.as_bytes().get(2).map(|b| char::from(*b).to_string())
}

/// A currency and amount field in `span`, with the file position in its error.
pub fn ccy_amount_in(
    fields: &Fields<'_>,
    span: Range<usize>,
    tag: &str,
    at: &At<'_>,
) -> Result<(Option<String>, Option<i128>), String> {
    match fields.find_in(span, tag) {
        Some(field) => {
            let (currency, amount) =
                ccy_amount(tag, &field.value).map_err(|e| format!("{}: {e}", at(field)))?;
            Ok((Some(currency), Some(amount)))
        }
        None => Ok((None, None)),
    }
}

/// Repeated field values as one text, newline-joined, or None when there are none.
pub fn joined(values: Vec<&str>) -> Option<String> {
    (!values.is_empty()).then(|| values.join("\n"))
}

/// A field's value, when the field is there at all.
pub fn text(field: Option<&Field<'_>>) -> Option<String> {
    field.map(|f| f.value.clone())
}

/// A party or institution field, as (identifier, account, credit/debit mark).
pub fn party(tag: &str, v: &str) -> (Option<String>, Option<String>, Option<String>) {
    let field = party_field(tag, v);
    (
        field.identifier.and_then(some_text),
        field.account.and_then(some_text),
        field.mark.map(str::to_string),
    )
}

/// One party field, decomposed. The layouts genuinely differ by option letter,
/// and the differences are load-bearing here: F numbers its lines and the rest
/// do not, G and A carry a BIC where H and K carry a name and an address.
pub fn party_field<'a>(tag: &str, v: &'a str) -> PartyField<'a> {
    let lines: Vec<&str> = v
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let option = tag.as_bytes().get(2).map(|b| char::from(*b));
    let mut out = PartyField::default();
    if lines.is_empty() {
        return out;
    }

    match option {
        // C: an account on every field but one. A `C` on field 50 is the BIC of
        // whoever instructed the payment, which `identifies` already knows and
        // this has to agree with: reading it as an account drops it, because a
        // caller asking for the instructing party asks for the identifier.
        Some('C') => match identifies(tag) {
            Identifies::Bic => out.identifier = Some(lines[0].trim_start_matches('/')),
            _ => out.account = Some(lines[0].trim_start_matches('/')),
        },
        // F: a party identifier, then lines numbered 1 to 8. Only 1, 2 and 3
        // carry the address; 4 to 8 are dates, places of birth and national
        // identifiers, which are about the person and not about where they are.
        Some('F') => {
            let (account, rest) = split_account(&lines);
            out.account = account;
            for line in rest {
                match line.as_bytes().first() {
                    Some(b'1') => {
                        let text = &line[2..];
                        // Repeated `1/` continues a name too long for one line.
                        if out.identifier.is_none() {
                            out.identifier = Some(text);
                        }
                    }
                    Some(b'2') => out.lines.push(&line[2..]),
                    Some(b'3') => {
                        let rest = &line[2..];
                        match rest.split_once('/') {
                            Some((country, town)) => {
                                out.country = Some(country);
                                out.town = (!town.is_empty()).then_some(town);
                            }
                            // A `3/` with no second slash states a country and
                            // no town, which is the shape the mandate refuses.
                            None => out.country = (!rest.is_empty()).then_some(rest),
                        }
                    }
                    _ => {}
                }
            }
            // A malformed F with no numbering at all still names somebody on its
            // first line, and reading nothing out of it would report a party
            // with no name rather than a party stated the wrong way.
            if out.identifier.is_none() && out.lines.is_empty() && out.country.is_none() {
                out.identifier = rest.first().copied();
                out.lines = rest.get(1..).unwrap_or_default().to_vec();
            }
        }
        // A, B, D, G, H and the letterless 59: an optional account, then the
        // identifier, then -- for the options that carry one -- its address.
        _ => {
            let (mark, account, rest) = match option {
                Some('A') | Some('B') | Some('D') => split_marked_account(&lines),
                _ => {
                    let (account, rest) = split_account(&lines);
                    (None, account, rest)
                }
            };
            out.mark = mark;
            out.account = account;
            out.identifier = rest.first().copied();
            if identifies(tag) == Identifies::Name {
                out.lines = rest.get(1..).unwrap_or_default().to_vec();
            }
        }
    }
    out
}

/// A leading `/account` line, and the lines after it.
fn split_account<'s, 'a>(lines: &'s [&'a str]) -> (Option<&'a str>, &'s [&'a str]) {
    match lines.first() {
        Some(first) if first.starts_with('/') => (
            non_empty(first.trim_start_matches('/')),
            lines.get(1..).unwrap_or_default(),
        ),
        _ => (None, lines),
    }
}

/// The same, for the party options that may prefix the account with the side it
/// is on: `/C/1234` is a credit account, `/1234` is just an account.
fn split_marked_account<'s, 'a>(
    lines: &'s [&'a str],
) -> (Option<&'a str>, Option<&'a str>, &'s [&'a str]) {
    let Some(first) = lines.first() else {
        return (None, None, &[]);
    };
    if !first.starts_with('/') {
        return (None, None, lines);
    }
    let rest = lines.get(1..).unwrap_or_default();
    let b = first.as_bytes();
    if b.len() > 3 && (b[1] == b'C' || b[1] == b'D') && b[2] == b'/' {
        (Some(&first[1..2]), non_empty(&first[3..]), rest)
    } else {
        (None, non_empty(first.trim_start_matches('/')), rest)
    }
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MT103: &str = concat!(
        "{1:F01NWBKGB2LAXXX0000000000}{2:I103BARCGB22XXXXN}{3:{108:MUR0001}{119:STP}",
        "{121:11111111-1111-4111-8111-111111111111}}{4:\n",
        ":20:TXREF001\n",
        ":23B:CRED\n",
        ":32A:260819EUR125,50\n",
        ":50K:/GB29NWBK60161331926819\n",
        "NORTHWIND RETAIL LTD\n",
        ":59A:/BE68539007547034\n",
        "UTILSUPPXXX\n",
        ":70:INVOICE RTP-1001\n",
        ":71A:SHA\n",
        "-}{5:{CHK:0123456789AB}}\n",
    );

    #[test]
    fn blocks_and_header_are_read_by_shape() {
        assert_eq!(message_type(MT103), Some("103"));
        assert_eq!(direction(MT103), Some("I"));
        assert_eq!(sender_bic(MT103).as_deref(), Some("NWBKGB2LXXX"));
        assert_eq!(receiver_bic(MT103).as_deref(), Some("BARCGB22XXX"));
        assert_eq!(user_header_field(MT103, "119"), Some("STP"));
        assert_eq!(user_header_field(MT103, "108"), Some("MUR0001"));
        assert_eq!(
            user_header_field(MT103, "121"),
            Some("11111111-1111-4111-8111-111111111111")
        );
        // Block 5 nests a {tag:value} of its own, so the close is by depth.
        assert_eq!(block(MT103, 5), Some("{CHK:0123456789AB}"));
    }

    #[test]
    fn a_field_value_runs_to_the_next_field_start() {
        let fields = Fields::parse(block(MT103, 4).unwrap());
        assert_eq!(fields.value("20"), Some("TXREF001"));
        // Two lines, joined; the second is not a field start.
        assert_eq!(
            fields.value("50"),
            Some("/GB29NWBK60161331926819\nNORTHWIND RETAIL LTD")
        );
        // A two-character key finds whichever option the message chose.
        assert_eq!(fields.find("50").map(|field| field.tag), Some("50K"));
        assert_eq!(fields.find("59").map(|field| field.tag), Some("59A"));
        // A longer key is the exact tag: 23B must not answer for 23E.
        assert_eq!(fields.value("23B"), Some("CRED"));
        assert_eq!(fields.value("23E"), None);
    }

    #[test]
    fn two_messages_in_one_file_are_two_messages() {
        let rje = format!("{MT103}$\n{MT103}");
        let mut reader = MtReader::new(rje.as_bytes(), "test");
        assert!(reader.next_message().unwrap().is_some());
        assert!(reader.next_message().unwrap().is_some());
        assert!(reader.next_message().unwrap().is_none());

        // Concatenated with no separator at all: block 1 already set is the cut.
        let glued = MT103.replace('\n', "").repeat(2);
        let mut reader = MtReader::new(glued.as_bytes(), "test");
        assert_eq!(
            message_type(&reader.next_message().unwrap().unwrap()),
            Some("103")
        );
        assert_eq!(
            message_type(&reader.next_message().unwrap().unwrap()),
            Some("103")
        );
        assert!(reader.next_message().unwrap().is_none());
    }

    #[test]
    fn a_bare_statement_file_frames_on_the_transaction_reference() {
        let bare = ":20:STMT-1\n:25:GB29NWBK60161331926819\n:60F:C260819EUR100,00\n-\n\
                    :20:STMT-2\n:25:GB29NWBK60161331926819\n:60F:C260820EUR200,00\n-\n";
        let mut reader = MtReader::new(bare.as_bytes(), "test");
        let first = reader.next_message().unwrap().unwrap();
        assert!(first.contains("STMT-1") && !first.contains("STMT-2"));
        let second = reader.next_message().unwrap().unwrap();
        assert!(second.contains("STMT-2"));
        assert!(reader.next_message().unwrap().is_none());
        // No blocks at all, so the body is the message.
        assert_eq!(message_type(&first), None);
        assert_eq!(Fields::parse(&first).value("20"), Some("STMT-1"));
    }

    #[test]
    fn amounts_use_a_comma_and_never_a_float() {
        assert_eq!(amount("125,50"), Ok(12_550_000));
        assert_eq!(amount("9,"), Ok(900_000));
        assert_eq!(amount("0,01"), Ok(1_000));
        assert!(amount("1.50").is_err());
        assert!(amount("1,2,3").is_err());
        assert!(amount("").is_err());
    }

    #[test]
    fn the_year_pivot_is_fixed() {
        // 68 is 2068 and 69 is 1969; the boundary does not move with the clock.
        assert_eq!(date2("680101"), date2_full("680101").map(|(d, _)| d));
        assert_eq!(date2_full("680101").map(|(_, y)| y), Some(2068));
        assert_eq!(date2_full("690101").map(|(_, y)| y), Some(1969));
        assert_eq!(date2("260819"), temporal::date_days("2026-08-19"));
        assert_eq!(date2("261301"), None);
    }

    #[test]
    fn an_entry_date_lands_on_the_near_side_of_the_year_boundary() {
        // Value date 1 January 2026, booked 31 December: the entry is 2025.
        let (jan, year) = date2_full("260101").unwrap();
        let entry = entry_date("1231", jan, year).unwrap();
        assert_eq!(entry, temporal::date_days("2025-12-31").unwrap());
        // Value date 31 December 2025, booked 1 January: the entry is 2026.
        let (dec, year) = date2_full("251231").unwrap();
        let entry = entry_date("0101", dec, year).unwrap();
        assert_eq!(entry, temporal::date_days("2026-01-01").unwrap());
        // Same year, no correction.
        let entry = entry_date("0818", date2("260819").unwrap(), 2026).unwrap();
        assert_eq!(entry, temporal::date_days("2026-08-18").unwrap());
    }

    #[test]
    fn a_statement_line_is_sliced_by_what_its_characters_are() {
        let line =
            statement_line("2608190818C125,50NTRFCUSTREF-1//BANKREF-1\nSUPPLEMENTARY DETAIL")
                .unwrap();
        assert_eq!(line.value_date, temporal::date_days("2026-08-19"));
        assert_eq!(
            line.entry_date,
            Some(temporal::date_days("2026-08-18").unwrap())
        );
        assert_eq!(line.credit_debit, "C");
        assert_eq!(line.funds_code, None);
        assert_eq!(line.amount, 12_550_000);
        assert_eq!(line.transaction_type.as_deref(), Some("N"));
        assert_eq!(line.transaction_code.as_deref(), Some("TRF"));
        assert_eq!(line.customer_ref.as_deref(), Some("CUSTREF-1"));
        assert_eq!(line.bank_ref.as_deref(), Some("BANKREF-1"));
        assert_eq!(line.supplementary.as_deref(), Some("SUPPLEMENTARY DETAIL"));

        // No entry date, and a funds code after the mark.
        let line = statement_line("260819CD74,25NMSCNONREF").unwrap();
        assert_eq!(line.entry_date, None);
        assert_eq!(line.credit_debit, "C");
        assert_eq!(line.funds_code.as_deref(), Some("D"));
        assert_eq!(line.customer_ref.as_deref(), Some("NONREF"));
        assert_eq!(line.bank_ref, None);

        // A reversal mark is two characters.
        let line = statement_line("260819RC10,00NTRFREV").unwrap();
        assert_eq!(line.credit_debit, "RC");
        assert_eq!(line.funds_code, None);

        // A single slash belongs to the customer reference; the split is on the
        // first double slash.
        let line = statement_line("260819D1,00NTRF341241773/1XXXXX//O/341241774").unwrap();
        assert_eq!(line.customer_ref.as_deref(), Some("341241773/1XXXXX"));
        assert_eq!(line.bank_ref.as_deref(), Some("O/341241774"));

        assert!(statement_line("").is_err());
        assert!(statement_line("260819N").is_err());
    }

    #[test]
    fn balances_and_amount_fields() {
        let b = balance("60F", "C260819EUR1234,56").unwrap();
        assert_eq!(
            (b.dc.as_str(), b.currency.as_str(), b.amount),
            ("C", "EUR", 123_456_000)
        );
        assert_eq!(b.date, temporal::date_days("2026-08-19"));
        assert!(balance("60F", "X260819EUR1,00").is_err());

        let (date, ccy, amt) = date_ccy_amount("260819EUR125,50").unwrap();
        assert_eq!(
            (date, ccy.as_str(), amt),
            (temporal::date_days("2026-08-19"), "EUR", 12_550_000)
        );

        assert_eq!(
            ccy_amount("71F", "EUR5,00"),
            Ok(("EUR".to_string(), 500_000))
        );
        assert_eq!(
            count_ccy_amount("2EUR199,75"),
            Ok((Some(2), "EUR".to_string(), 19_975_000))
        );
        assert_eq!(statement_number("00123/2"), (Some(123), Some(2)));
        assert_eq!(statement_number("7"), (Some(7), None));
    }

    #[test]
    fn each_party_option_has_its_own_layout() {
        assert_eq!(
            party("52A", "/C/12345\nNWBKGB2LXXX"),
            (
                Some("NWBKGB2LXXX".into()),
                Some("12345".into()),
                Some("C".into())
            )
        );
        assert_eq!(
            party("57A", "/98765\nBARCGB22XXX"),
            (Some("BARCGB22XXX".into()), Some("98765".into()), None)
        );
        assert_eq!(
            party("56A", "DEUTDEFFXXX"),
            (Some("DEUTDEFFXXX".into()), None, None)
        );
        assert_eq!(
            party("57C", "//CH123456"),
            (None, Some("CH123456".into()), None)
        );
        assert_eq!(
            party("52D", "/D/55555\nBANK OF SOMEWHERE\n1 HIGH STREET"),
            (
                Some("BANK OF SOMEWHERE".into()),
                Some("55555".into()),
                Some("D".into())
            )
        );
        assert_eq!(
            party("50K", "/GB29NWBK60161331926819\nNORTHWIND RETAIL LTD"),
            (
                Some("NORTHWIND RETAIL LTD".into()),
                Some("GB29NWBK60161331926819".into()),
                None
            )
        );
        assert_eq!(
            party("59", "/BE68539007547034\nUTILITY SUPPLIER SA"),
            (
                Some("UTILITY SUPPLIER SA".into()),
                Some("BE68539007547034".into()),
                None
            )
        );
        // The option letter is not enough on its own. A `C` on field 50 is the BIC
        // of whoever instructed the payment and belongs in the identifier; a `C`
        // anywhere else is an account and nothing else. Reading 50C as an account
        // drops it, because every caller asking for an instructing party asks for
        // the identifier.
        assert_eq!(
            party("50C", "NWBKGB2LXXX"),
            (Some("NWBKGB2LXXX".into()), None, None)
        );
        assert_eq!(
            party("52C", "/98765"),
            (None, Some("98765".into()), None),
            "the same letter on a bank field is an account"
        );
        assert_eq!(
            party(
                "50F",
                "/GB29NWBK60161331926819\n1/NORTHWIND RETAIL LTD\n2/1 HIGH STREET"
            ),
            (
                Some("NORTHWIND RETAIL LTD".into()),
                Some("GB29NWBK60161331926819".into()),
                None
            )
        );
        assert_eq!(party("52A", ""), (None, None, None));
    }

    /// The parts every reader here drops. `:50K:` lines 2 onward are an address
    /// and always have been; `:50F:` is the one option that states the town and
    /// the country where a translator can find them, which is why the 14 November
    /// 2026 rule is survivable in MT at all.
    #[test]
    fn a_party_field_carries_an_address_the_readers_do_not_keep() {
        // Real, from prowide-core's MT103-out-ack.rje.
        let k = party_field(
            "50K",
            "/22222222222\nOLD MUTUAL GENERAL INSURAN\n226 AWOLOWO WAY 322\nLAGOS NIGERIA",
        );
        assert_eq!(identifies("50K"), Identifies::Name);
        assert_eq!(k.identifier, Some("OLD MUTUAL GENERAL INSURAN"));
        assert_eq!(k.account, Some("22222222222"));
        assert_eq!(k.lines, ["226 AWOLOWO WAY 322", "LAGOS NIGERIA"]);
        // Lagos and Nigeria are in there, and no element says so.
        assert_eq!((k.town, k.country, k.structured()), (None, None, 0));

        // Real, from prowide-core's MT101.fin: option H, the same free-text shape.
        let h = party_field("50H", "/344110001637\nTESTAR00AXXX\nUtrecht\nNetherlands");
        assert_eq!(h.identifier, Some("TESTAR00AXXX"));
        assert_eq!(h.lines, ["Utrecht", "Netherlands"]);
        assert_eq!(h.structured(), 0);

        let f = party_field(
            "59F",
            "/BE30001216371411\n1/JOHN SMITH\n2/HOOGSTRAAT 6\n3/BE/BRUSSELS",
        );
        assert_eq!(f.identifier, Some("JOHN SMITH"));
        assert_eq!(f.lines, ["HOOGSTRAAT 6"]);
        assert_eq!(
            (f.town, f.country, f.structured()),
            (Some("BRUSSELS"), Some("BE"), 2)
        );

        // Subfield 1 may be a code and a country instead of an account, and
        // subfields 4 to 8 are about the person rather than the place.
        let coded = party_field(
            "50F",
            "CCPT/GB/123456789\n1/JOHN SMITH\n2/1 HIGH STREET\n3/GB/LONDON\n4/19700101\n5/GB/LONDON",
        );
        assert_eq!(coded.account, None);
        assert_eq!(coded.lines, ["1 HIGH STREET"]);
        assert_eq!((coded.town, coded.country), (Some("LONDON"), Some("GB")));

        // A country with no town: stated separately and still not enough.
        let bare = party_field("50F", "/1\n1/A NAME\n3/DE");
        assert_eq!(
            (bare.town, bare.country, bare.structured()),
            (None, Some("DE"), 1)
        );

        // A BIC is not a name and has no address under it.
        let a = party_field("57A", "/98765\nBARCGB22XXX");
        assert_eq!(identifies("57A"), Identifies::Bic);
        assert!(a.lines.is_empty());

        // Option B names a place inside the sender's own bank, not a party.
        let b = party_field("53B", "/C/12345\nFRANKFURT BRANCH");
        assert_eq!(identifies("53B"), Identifies::Location);
        assert_eq!(b.mark, Some("C"));
        assert!(b.lines.is_empty());

        // The option letter alone does not settle it. A `C` on field 50 is the
        // BIC of whoever instructed the payment; on 52 and 57 it is an account
        // number with nobody's name attached.
        assert_eq!(identifies("50C"), Identifies::Bic);
        assert_eq!(identifies("52C"), Identifies::Nothing);
        assert_eq!(identifies("57C"), Identifies::Nothing);
        assert_eq!(identifies("50L"), Identifies::Name);
        assert_eq!(identifies("59"), Identifies::Name);
    }

    #[test]
    fn a_message_without_a_boundary_is_refused() {
        let mut text = String::from(":20:NOBOUNDARY\n");
        while text.len() < 512 {
            text.push_str("A LINE THAT IS NOT A FIELD START AND NOT A TERMINATOR\n");
        }
        let mut reader = MtReader::with_limit(text.as_bytes(), "t", 256);
        let error = reader.next_message().unwrap_err().to_string();
        assert!(
            error.contains("no MT message boundary in the first 256 bytes"),
            "{error}"
        );
    }

    #[test]
    fn a_service_envelope_is_not_a_message() {
        // An ACK out of prowide's MT103-bulk-with-ack.rje: block 1 and no block 2.
        let ack = "{1:F21AAAAUSLAAXXX5195167828}{4:{177:1704260717}{451:0}}";
        let ack_fields = Fields::parse(block(ack, 4).unwrap_or(ack));
        for mt in ["103", "202", "940", "942"] {
            assert!(!claims(ack, &ack_fields, mt), "an ACK claimed by MT{mt}");
        }

        // A bare statement body is claimed by the reader whose mandatory field
        // it carries, and by no other.
        let bare = ":20:STMT-1\n:25:GB29NWBK60161331926819\n:60F:C260819EUR100,00\n";
        let fields = Fields::parse(bare);
        assert!(claims(bare, &fields, "940"));
        for mt in ["103", "202", "942"] {
            assert!(!claims(bare, &fields, mt), "a bare body claimed by MT{mt}");
        }
    }

    #[test]
    fn an_entry_date_may_be_padded_with_spaces() {
        // wolph/mt940 mt940_tests/citi/mt940.txt, and the ASNB file beside it.
        let line = statement_line("200101    D65,00NOVBNL47INGB9999999999").unwrap();
        assert_eq!(line.entry_date, None);
        assert_eq!(line.credit_debit, "D");
        assert_eq!(line.funds_code, None);
        assert_eq!(line.amount, 6_500_000);
        assert_eq!(line.transaction_type.as_deref(), Some("N"));
        assert_eq!(line.transaction_code.as_deref(), Some("OVB"));
        assert_eq!(line.customer_ref.as_deref(), Some("NL47INGB9999999999"));
    }

    #[test]
    fn an_unreadable_date_is_null_not_an_error() {
        // prowide-core src/test/resources/sample_JPchar.txt: value date 345454.
        let line = statement_line("3454543545CY1234,NTRFNONREF//AABB-01111").unwrap();
        assert_eq!(line.value_date, None);
        assert_eq!(line.entry_date, None);
        assert_eq!(line.amount, 123_400_000);
        assert_eq!(line.credit_debit, "C");
        assert_eq!(line.funds_code.as_deref(), Some("Y"));
        assert_eq!(date2("345454"), None);
    }

    #[test]
    fn an_86_narrative_is_one_wrapped_value() {
        // wolph/mt940 mt940_tests/betterplace/with_binary_character.sta: the
        // wrap falls inside ABBUCHUNG.
        let body = concat!(
            ":61:1003190319CR27,00NTRF100323-03-100323//32-P1-TCS49518\n",
            ":86:051?00Bank Transfer Credit?10930226?20QQW53T2245ZGY46J ABBUCH\n",
            "UNG?21VOM PAYPAL-KONTO?22100318P3TX1433EV?3050070010?310175526300\n",
            "?32PAYPAL?34000\n",
        );
        let narrative = Fields::parse(body).value("86").unwrap().to_string();
        assert!(narrative.contains("ABBUCHUNG"), "{narrative}");
        assert!(!narrative.contains('\n'), "{narrative}");

        // A name-and-address field breaks where the writer meant it to.
        let fields = Fields::parse(":50K:/GB29NWBK60161331926819\nNORTHWIND RETAIL LTD\n");
        assert_eq!(fields.value("50").unwrap().matches('\n').count(), 1);
    }

    #[test]
    fn a_terminator_with_trailing_junk_still_terminates() {
        // prowide-core src/test/resources/MT103-out-ack.rje ends its block 4
        // with `-}{`.
        let fields = Fields::parse(":20:TXREF001\n:71A:OUR\n-}{\n");
        assert_eq!(fields.value("71A"), Some("OUR"));
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn a_field_knows_its_line() {
        let fields = Fields::parse(block(MT103, 4).unwrap());
        assert_eq!(fields.find("32A").unwrap().line, 3);
        // A field on body line 3, of a body starting on message line 2, of a
        // message starting on file line 10.
        assert_eq!(at(10, 2, 3), 13);
    }

    #[test]
    fn an_entry_is_a_region_of_the_body() {
        let body = concat!(
            ":20:STMT-1\n",
            ":60F:C260819EUR100,00\n",
            ":61:2608190819C10,00NTRFREF-1\n",
            "/OCMT/EUR10,00/\n",
            ":86:FIRST NARRATIVE\n",
            ":61:2608190819D5,00NTRFREF-2\n",
            ":86:SECOND ONE, WRAPPED\n",
            "AND CONTINUED\n",
            ":62F:C260819EUR105,00\n",
            ":86:STATEMENT NARRATIVE\n",
        );
        let (fields, entries) = Fields::without_entries(body, "61");
        // The statement keeps its own fields and none of the entries'.
        assert_eq!(fields.len(), 4);
        assert!(fields.find("61").is_none());
        assert_eq!(fields.value("86"), Some("STATEMENT NARRATIVE"));
        assert_eq!(fields.value("62"), Some("C260819EUR105,00"));
        assert_eq!(entries, 2);

        // The cursor hands the regions over one at a time and keeps none of them.
        let mut cursor = EntryCursor::default();
        let first_site = cursor.next_site(body, "61").expect("a first entry");
        let second_site = cursor.next_site(body, "61").expect("a second entry");
        assert!(cursor.next_site(body, "61").is_none());
        assert_eq!(first_site.line, 3);
        assert_eq!(second_site.line, 6);
        // The cursor reads each line of the body once and never goes back, so
        // finding every region costs one pass and not one per entry.
        assert_eq!(cursor.seen, body.split('\n').count());

        // An entry region parses to its own statement line and narrative, and the
        // `:86:` wrap is joined without a newline the way field 86 is everywhere.
        let first = Fields::parse(&body[first_site.bytes.clone()]);
        assert_eq!(
            first.value("61"),
            Some("2608190819C10,00NTRFREF-1\n/OCMT/EUR10,00/")
        );
        assert_eq!(first.all("86"), vec!["FIRST NARRATIVE"]);
        let second = Fields::parse(&body[second_site.bytes.clone()]);
        assert_eq!(second.all("86"), vec!["SECOND ONE, WRAPPEDAND CONTINUED"]);

        // `parse` is the same walker with no entry tag, so it still sees them all.
        assert_eq!(Fields::parse(body).len(), 8);
    }

    /// An `:86:` under a `:61:` that sits after the closing balance belongs to
    /// that entry. A field walk would have given it to the statement narrative,
    /// because it comes after `:62:`; the region split gives it to the entry
    /// above it. No file in `testdata/` or either fetched corpus writes a `:61:`
    /// after its `:62:`, so this test is what holds the choice.
    #[test]
    fn an_86_under_an_entry_after_the_closing_balance_stays_on_the_entry() {
        let body = concat!(
            ":20:STMT-1\n",
            ":60F:C260819EUR100,00\n",
            ":61:2608190819C10,00NTRFREF-1\n",
            ":86:UNDER THE FIRST ENTRY\n",
            ":62F:C260819EUR105,00\n",
            ":61:2608190819D5,00NTRFREF-2\n",
            ":86:AFTER THE CLOSING BALANCE\n",
        );
        let (fields, entries) = Fields::without_entries(body, "61");
        assert_eq!(entries, 2);
        // No `:86:` reaches the statement, so `mt940::statement_narrative`, which
        // takes the ones after `:62:`, has nothing to take.
        assert!(fields.all("86").is_empty());

        let mut cursor = EntryCursor::default();
        let first = cursor.next_site(body, "61").expect("a first entry");
        let second = cursor.next_site(body, "61").expect("a second entry");
        assert_eq!(
            Fields::parse(&body[first.bytes.clone()]).all("86"),
            vec!["UNDER THE FIRST ENTRY"]
        );
        assert_eq!(
            Fields::parse(&body[second.bytes.clone()]).all("86"),
            vec!["AFTER THE CLOSING BALANCE"]
        );
        assert!(cursor.next_site(body, "61").is_none());
        assert_eq!(cursor.seen, body.split('\n').count());
    }
}
