//! `audit_addresses` - every party on the wire beside the shape of its postal
//! address, classified against the rule that takes effect on 14 November 2026.
//!
//! From that date CBPR+ refuses a fully unstructured postal address. Town and
//! country have to sit in a `<TwnNm>` and a `<Ctry>` of their own, and the rest
//! is either dedicated elements throughout or at most two `<AdrLine>` of at most
//! 70 characters beside them. So a bank has one question to answer before the
//! date, and it is a data question rather than a messaging one: of the traffic
//! already on disk, which parties would be refused, and where do they come from.
//!
//! The readers cannot answer it. A party is one column of a transaction row
//! there - `debtor_name` - and the address is not on the row at all: pacs.008
//! alone carries five parties and six agents that may hold one, so putting the
//! address on the transaction row would be forty columns serving a question
//! nobody asks per transaction. The grain is wrong as well. This is one row per
//! party occurrence, which is what makes a folder of mixed messages group by
//! role, by country and by format in a single query.
//!
//! Which columns are facts and which is a verdict: `address_format`, the three
//! counts and the leaves are read off the wire. `finding` is the rule applied to
//! them, and NULL means nothing in this party would be refused. Scope is decided
//! by family, because the mandate excludes the cash-management and
//! administration messages ([`OUT_OF_SCOPE`]); a `finding` is never raised
//! against those, and `family` is a column so a caller can see which side of
//! that line a row sits on.
//!
//! An agent identified by a BIC does not need an address at all, so a BIC-only
//! agent is `NONE` with no finding. A party carrying no address is also `NONE`
//! with no finding: whether it was required there is a usage-guideline question
//! this cannot see, and inventing a verdict for it would bury the rows that
//! genuinely break.
//!
//! SWIFT MT is read here too, and it is where the question comes from: an MT
//! `:50K:` is a name and then free-text address lines, which is the shape the
//! mandate refuses, and `:50F:` is the one option that states the town and the
//! country in a subfield a translator can find. So the same query answers the
//! migration question over a folder holding both: which parties, in whichever
//! format they arrived, would be refused. `family` says which side they came
//! from - `pacs.008` or `mt.103` - and the classification is one code path, so
//! the two cannot disagree about what UNSTRUCTURED means.
//!
//! MT numbers its transactions differently in every message type, so
//! `record_index` is NULL for MT rather than guessed, and `party_path` carries
//! the field tag with `#2` on a repeat: `50K`, `52A`, `52A#2`. The audit reads
//! MT types no reader here covers, MT101 and MT210 among them, because a party
//! field is a party field whether or not the rest of the message is understood.
//!
//! Peak memory is one party, which holds its own leaves and nothing else, plus
//! one output batch. Parties are emitted as their closing tag is read, so a
//! message of half a million transactions costs what one of them costs.

use std::collections::VecDeque;
use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::mt;
use crate::sniff::{
    family_of_container, family_of_identifier, find_identifier, identifier_ns,
    is_message_definition, is_message_id, message_definition, RECORD_ELEMS,
};
use crate::wire;

/// Parties that carry a postal address. The case family's `Assgnr` and `Assgne`
/// are not among them: an assigner is the bank handling an investigation, not a
/// party to the payment, and the mandate is about the payment. A camt.056 still
/// yields rows, because the payment parties it copies into `OrgnlTxRef` are
/// spelled `Dbtr` and `Cdtr` like everywhere else.
///
/// `Pyer` and `Pyee` are the cheque messages' own spelling of the two ends:
/// camt.107, camt.108 and camt.109 name nobody `Dbtr` or `Cdtr`, so without
/// these two a cheque presentment audited as zero parties - which reads exactly
/// like a clean file. Roles are added when the pinned corpus states them and
/// not on the strength of a schema: an inferred role nobody sends is a column
/// nothing can be wrong about.
const PARTY_ROLES: [&str; 7] = [
    "InitgPty",
    "Dbtr",
    "Cdtr",
    "UltmtDbtr",
    "UltmtCdtr",
    "Pyer",
    "Pyee",
];

/// Agents that carry one. An agent named by BIC alone does not need an address;
/// one that states an address anyway has to state it the new way. `DrwrAgt` is
/// the drawer's bank on a cheque, which is the agent the cheque messages state.
const AGENT_ROLES: [&str; 12] = [
    "InstgAgt",
    "InstdAgt",
    "DbtrAgt",
    "CdtrAgt",
    "DrwrAgt",
    "IntrmyAgt1",
    "IntrmyAgt2",
    "IntrmyAgt3",
    "PrvsInstgAgt1",
    "PrvsInstgAgt2",
    "PrvsInstgAgt3",
    "FwdgAgt",
];

/// The dedicated elements of `PostalAddress`, `AdrTp` excluded: a type
/// indicator says nothing about where the party is, so counting it would report
/// a structured element that carries no address.
const ADDRESS_ELEMS: [&str; 16] = [
    "CareOf",
    "Dept",
    "SubDept",
    "StrtNm",
    "BldgNb",
    "BldgNm",
    "Flr",
    "UnitNb",
    "PstBx",
    "Room",
    "PstCd",
    "TwnNm",
    "TwnLctnNm",
    "DstrctNm",
    "CtrySubDvsn",
    "Ctry",
];

/// Families the address mandate does not reach: the cash-management reports and
/// the administration messages. Their parties are still reported, with the
/// format they are in, and never a finding.
const OUT_OF_SCOPE: [&str; 6] = [
    "admi.024", "camt.025", "camt.052", "camt.053", "camt.054", "camt.060",
];

/// At most this many `<AdrLine>` in the hybrid form, and at most this many
/// characters in each.
const HYBRID_LINES: i64 = 2;
const HYBRID_LINE_CHARS: i64 = 70;

#[derive(Debug, Default, Clone)]
pub struct AddrRow {
    pub family: Option<String>,
    pub message_id: Option<String>,
    /// Which transaction of the message this party belongs to, 1-based. NULL for
    /// a party stated once for the whole message or the whole payment group.
    pub record_index: Option<i64>,
    /// Where the party sits: the record element it is in, then its own tag.
    pub party_path: Option<String>,
    pub role: Option<String>,
    pub party_kind: Option<String>,
    pub name: Option<String>,
    pub bic: Option<String>,
    pub town: Option<String>,
    pub country: Option<String>,
    /// The address lines as they stand on the wire, one per line. A refusal names
    /// what is missing; this is what is there to fix, which the counts alone do
    /// not give. NULL when the party carries no line at all.
    pub address_text: Option<String>,
    pub address_lines: Option<i64>,
    pub longest_address_line: Option<i64>,
    pub structured_elements: Option<i64>,
    pub address_format: Option<String>,
    pub finding: Option<String>,
    pub source_file: Option<String>,
}

/// The party the walk is inside, with the leaves read so far. One of these is
/// alive at a time.
struct Party {
    /// `path.len()` at the party's start element, before it was pushed.
    depth: usize,
    role: String,
    kind: &'static str,
    party_path: String,
    record_index: Option<i64>,
    name: Option<String>,
    bic: Option<String>,
    town: Option<String>,
    country: Option<String>,
    structured: i64,
    /// One entry per address line on the wire, in order, blank ones included: the
    /// count and the longest are read off this rather than tallied beside it.
    line_text: Vec<String>,
}

impl Party {
    /// How many address lines are on the wire. A blank line and a self-closing one
    /// both count: the limit of two is a limit on lines, not on content.
    fn lines(&self) -> i64 {
        self.line_text.len() as i64
    }

    /// The longest line in characters, which is the limit the rule states.
    fn longest(&self) -> i64 {
        self.line_text
            .iter()
            .map(|l| l.chars().count() as i64)
            .max()
            .unwrap_or(0)
    }

    /// STRUCTURED, HYBRID, UNSTRUCTURED or NONE, off the wire alone.
    fn format(&self) -> &'static str {
        let located = self.town.is_some() && self.country.is_some();
        match (self.lines(), self.structured, located) {
            (0, 0, _) => "NONE",
            (0, _, _) => "STRUCTURED",
            (_, _, true) => "HYBRID",
            _ => "UNSTRUCTURED",
        }
    }

    /// Why the 14 November 2026 rule would refuse this party, or None.
    fn finding(&self, format: &str) -> Option<String> {
        let mut faults: Vec<String> = Vec::new();
        match format {
            "NONE" => return None,
            "UNSTRUCTURED" => {
                faults.push(
                    match (self.town.is_some(), self.country.is_some()) {
                        (false, false) => "address lines with no TwnNm and no Ctry",
                        (false, true) => "address lines with no TwnNm",
                        (true, false) => "address lines with no Ctry",
                        // A located party is HYBRID, so this arm is unreachable.
                        (true, true) => "address lines",
                    }
                    .to_string(),
                );
            }
            _ => {
                if self.town.is_none() {
                    faults.push("no TwnNm".to_string());
                }
                if self.country.is_none() {
                    faults.push("no Ctry".to_string());
                }
            }
        }
        if self.lines() > HYBRID_LINES {
            faults.push(format!(
                "{} address lines, at most {HYBRID_LINES} permitted",
                self.lines()
            ));
        }
        if self.longest() > HYBRID_LINE_CHARS {
            faults.push(format!(
                "an address line of {} characters, at most {HYBRID_LINE_CHARS} permitted",
                self.longest()
            ));
        }
        (!faults.is_empty()).then(|| faults.join("; "))
    }
}

pub struct AddressStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    /// Rows finished but not yet handed out. A party closes at most one row, so
    /// this holds at most one; it is a queue so the walk never has to hold a row
    /// and a half-read party at once.
    queue: VecDeque<AddrRow>,
    /// The family of the message being walked, and the one an enclosing envelope
    /// declared. A file may hold several messages of different families, so the
    /// first is per message and the second is the fallback each of them starts
    /// from.
    family: Option<String>,
    /// What the enclosing `<Document>` bound, and what an envelope above it did.
    /// The Document's binding wins over the container name, the way the sniffer
    /// resolves identity; the envelope's is the last resort.
    document_family: Option<String>,
    envelope_family: Option<String>,
    /// What an `AppHdr/MsgDefIdr` above the message named. Pending rather than
    /// established: a header states what its payload is, and a file may carry a
    /// header and then no payload at all, so this identifies a message without
    /// being one. It ranks below the message namespace and above the container
    /// name, exactly as it does in the sniffer.
    header_family: Option<String>,
    message_id: Option<String>,
    /// `path.len()` at the start element of the message being walked. A file may
    /// hold several complete messages - an envelope with two `<Document>`s, or a
    /// Document-less envelope with two containers - and the identity and the
    /// transaction numbering belong to one of them, not to the file. This is what
    /// says which. ADR 0007 names the same trap on the pacs.028 side.
    message_depth: Option<usize>,
    /// `path.len()` at the `<Document>` the walk is inside, so that a Document
    /// whose own child is nothing this recognises can still be the message.
    document_depth: Option<usize>,
    /// Set when `<Document>` is open and its first child has not been seen yet.
    awaiting_child: bool,
    /// Whether anything identifying an ISO 20022 message was found at all.
    identified: bool,
    /// The record element the walk is inside, and which one it is.
    record_index: i64,
    record_depth: Option<usize>,
    record_name: Option<String>,
    open: Option<Party>,
}

impl<R: BufRead> AddressStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        AddressStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            queue: VecDeque::new(),
            family: None,
            document_family: None,
            envelope_family: None,
            header_family: None,
            message_id: None,
            message_depth: None,
            document_depth: None,
            awaiting_child: false,
            identified: false,
            record_index: 0,
            record_depth: None,
            record_name: None,
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AddrRow>, Box<dyn Error>> {
        loop {
            if let Some(row) = self.queue.pop_front() {
                return Ok(Some(row));
            }
            self.buf.clear();
            let ev = wire::next_event(&mut self.reader, &mut self.buf, &self.path, &self.source)?;
            // A self-closing element has no matching End, so it never enters the
            // path and is never a record or a party - but it is an element on the
            // wire, which is what an address line is counted as. The event is
            // decoded before anything is done with it because it borrows the read
            // buffer, and the buffer is a field of the state being updated.
            let push = matches!(ev, Event::Start(_));
            let act = match ev {
                Event::Eof => Act::Eof,
                Event::Start(e) | Event::Empty(e) => {
                    let name = wire::local(e.name().as_ref()).into_owned();
                    let ns_family = identifier_ns(&e)
                        .as_deref()
                        .and_then(find_identifier)
                        .map(|ident| family_of_identifier(ident).to_string());
                    Act::Element(name, ns_family)
                }
                Event::End(_) => Act::Pop,
                ev => match wire::event_text(&ev)? {
                    Some(text) => Act::Text(text),
                    None => Act::None,
                },
            };

            match act {
                Act::Eof => {
                    return if self.identified {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no ISO 20022 message found — is this an ISO 20022 file?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Element(name, ns_family) => {
                    self.element(&name, ns_family, push);
                    if push {
                        self.path.push(name);
                    }
                }
                Act::Pop => {
                    self.path.pop();
                    if self
                        .open
                        .as_ref()
                        .is_some_and(|p| self.path.len() == p.depth)
                    {
                        let party = self.open.take().expect("checked just above");
                        self.queue.push_back(row(
                            party,
                            self.family.as_deref(),
                            self.message_id.as_deref(),
                            &self.source,
                        ));
                    }
                    if self.record_depth.is_some_and(|d| self.path.len() <= d) {
                        self.record_depth = None;
                        self.record_name = None;
                    }
                    if self.message_depth == Some(self.path.len()) {
                        self.message_depth = None;
                    }
                }
                Act::Text(text) => self.capture(&text),
                Act::None => {}
            }
        }
    }

    /// One element start, self-closing or not: message boundaries, identity, the
    /// record being counted, and the party being opened.
    fn element(&mut self, name: &str, ns_family: Option<String>, push: bool) {
        // `document_family` is the Document's own binding and nothing else. Its
        // first child was once read into it as a fallback, which said the same
        // thing for that child and the wrong thing for the next one: a second
        // container of another family under one namespace-free Document
        // inherited the first one's family.
        if self.awaiting_child {
            // A child this does not recognise means no container will claim the
            // message, so the Document is the message: without that, a mapped
            // name nested anywhere inside a national wrapper - `Rcpt` above all
            // - would open a second message and take the identity with it.
            //
            // Only when something named the Document, though. A Document with no
            // binding, no header and no envelope above it has no identity to
            // protect, and claiming the scope there would stop the container
            // underneath from naming the family at all.
            let named = self.document_family.is_some()
                || self.header_family.is_some()
                || self.envelope_family.is_some();
            if named && family_of_container(name).is_none() && self.message_depth.is_none() {
                self.message_depth = self.document_depth;
            }
            self.awaiting_child = false;
        }
        if name == "Document" {
            self.identified = true;
            self.awaiting_child = true;
            self.document_family = ns_family.clone();
            self.document_depth = Some(self.path.len());
            // A Document is a wrapper and not the message: the message begins at
            // its container, below. Reset here anyway, so that a Document holding
            // a container this does not know is still one message per Document.
            let family = self
                .document_family
                .clone()
                .or_else(|| self.header_family.clone())
                .or_else(|| self.envelope_family.clone());
            self.begin_message(None, family);
        } else if push && self.open.is_none() && self.message_depth.is_none() {
            // The container IS the message. One Document may hold several
            // complete ones - `testdata/pacs002_two_reports.xml` holds two status
            // reports - and an envelope may carry them with no Document at all,
            // so the boundary is here rather than at the wrapper. Only the
            // outermost one opens: `Rcpt` is a name a message of another family
            // may well use for something of its own, and a message already under
            // way is not restarted by an element inside it.
            if let Some(container) = family_of_container(name) {
                self.identified = true;
                let family = self
                    .document_family
                    .clone()
                    .or_else(|| self.header_family.clone())
                    .or_else(|| Some(container.to_string()));
                self.begin_message(Some(self.path.len()), family);
            }
        }
        // A namespace declared above a message belongs to the envelope, and every
        // message inside it starts from there.
        if let Some(family) = ns_family {
            self.identified = true;
            if name != "Document" && self.message_depth.is_none() && self.envelope_family.is_none()
            {
                self.envelope_family = Some(family.clone());
            }
            if self.family.is_none() {
                self.family = Some(family);
            }
        }
        if self.open.is_some() {
            // Inside a party, the only element that is a fact of its own is an
            // address line: opened here, as an element, because the limit of two
            // is a limit on how many are on the wire. A blank line and a
            // self-closing one are both a line; what they are not is content,
            // which is why `structured_elements` counts populated elements. The
            // entry starts empty and the text event fills it.
            if name == "AdrLine" && self.in_postal_address() {
                if let Some(party) = self.open.as_mut() {
                    party.line_text.push(String::new());
                }
            }
            return;
        }
        if !push {
            return;
        }
        if RECORD_ELEMS.contains(&name) {
            self.record_index += 1;
            self.record_depth = Some(self.path.len());
            self.record_name = Some(name.to_string());
        }
        if let Some(kind) = role_kind(name) {
            self.open = Some(Party {
                depth: self.path.len(),
                party_path: self.party_path(name),
                role: name.to_string(),
                kind,
                record_index: self.record_depth.map(|_| self.record_index),
                name: None,
                bic: None,
                town: None,
                country: None,
                structured: 0,
                line_text: Vec::new(),
            });
        }
    }

    /// A new message begins: its identity and its transaction numbering are its
    /// own, and start from whatever identified the message around them.
    fn begin_message(&mut self, depth: Option<usize>, family: Option<String>) {
        self.message_depth = depth;
        self.family = family;
        self.message_id = None;
        self.record_index = 0;
        self.record_depth = None;
        self.record_name = None;
    }

    /// Where the party sits: from the record element it is in down to its own
    /// tag, or its tag alone when it is stated for the whole message.
    ///
    /// The whole tail and not just the two ends, because that is what tells a
    /// party of *this* message from one it copied out of the payment it answers:
    /// a cancellation's own creditor is `TxInf/Cdtr`, and the creditor of the
    /// payment being cancelled is `TxInf/OrgnlTxRef/Cdtr`.
    fn party_path(&self, name: &str) -> String {
        let mut out = String::new();
        if let Some(depth) = self.record_depth {
            for element in &self.path[depth..] {
                out.push_str(element);
                out.push('/');
            }
        }
        out.push_str(name);
        out
    }

    /// Whether the cursor is inside the open party's `PstlAdr`. `Ctry` and `TwnNm`
    /// are only an address in there: a party may state a country of residence of
    /// its own.
    fn in_postal_address(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|p| self.path[p.depth..].iter().any(|e| e == "PstlAdr"))
    }

    /// A leaf's text, to the party being read or to the message identity.
    fn capture(&mut self, text: &str) {
        enum Slot {
            Town,
            Country,
            Structured,
            Line,
            Name,
            Bic,
        }

        let Some(last) = self.path.last() else {
            return;
        };
        if self.open.is_none() {
            if self.message_id.is_none() && is_message_id(&self.path) {
                self.message_id = Some(text.to_string());
            }
            // The header is read wherever it sits, including above any message,
            // and it does not set `identified`: a file that is only a header
            // still has no message in it.
            if self.header_family.is_none() && is_message_definition(&self.path) {
                self.header_family =
                    message_definition(text).map(|ident| family_of_identifier(ident).to_string());
            }
            return;
        }
        let slot = if self.in_postal_address() {
            match last.as_str() {
                "TwnNm" => Slot::Town,
                "Ctry" => Slot::Country,
                "AdrLine" => Slot::Line,
                other if ADDRESS_ELEMS.contains(&other) => Slot::Structured,
                _ => return,
            }
        } else {
            match last.as_str() {
                "Nm" => Slot::Name,
                "BICFI" | "BIC" | "AnyBIC" => Slot::Bic,
                _ => return,
            }
        };

        let party = self.open.as_mut().expect("open, checked above");
        match slot {
            // The entry was opened at the start element. Text is appended rather
            // than assigned because an entity or a CDATA section splits one line's
            // text across several events.
            Slot::Line => {
                if let Some(line) = party.line_text.last_mut() {
                    line.push_str(text);
                }
            }
            Slot::Town => {
                party.structured += 1;
                party.town.get_or_insert_with(|| text.to_string());
            }
            Slot::Country => {
                party.structured += 1;
                party.country.get_or_insert_with(|| text.to_string());
            }
            Slot::Structured => party.structured += 1,
            Slot::Name => {
                party.name.get_or_insert_with(|| text.to_string());
            }
            Slot::Bic => {
                party.bic.get_or_insert_with(|| text.to_string());
            }
        }
    }
}

/// One party, as a row. Both walks end here: the format, the scope test and the
/// verdict are decided in one place so an MT party and an ISO 20022 party cannot
/// be told apart by anything except what their messages actually said.
fn row(party: Party, family: Option<&str>, message_id: Option<&str>, source: &str) -> AddrRow {
    let format = party.format();
    let in_scope = !family.is_some_and(|f| OUT_OF_SCOPE.contains(&f));
    let finding = in_scope.then(|| party.finding(format)).flatten();
    // Read off the party before the struct literal moves its fields out.
    let address_text = (!party.line_text.is_empty()).then(|| party.line_text.join("\n"));
    let (lines, longest) = (party.lines(), party.longest());
    AddrRow {
        family: family.map(str::to_string),
        message_id: message_id.map(str::to_string),
        record_index: party.record_index,
        party_path: Some(party.party_path),
        role: Some(party.role),
        party_kind: Some(party.kind.to_string()),
        name: party.name,
        bic: party.bic,
        town: party.town,
        country: party.country,
        address_text,
        address_lines: Some(lines),
        longest_address_line: Some(longest),
        structured_elements: Some(party.structured),
        address_format: Some(format.to_string()),
        finding,
        source_file: Some(source.to_string()),
    }
}

/// The audit over one file, whichever wire format it turned out to be.
///
/// The guard has already read the prefix and decided, so this is a choice and
/// not a probe: by the time a walk opens, the shape is known.
///
/// The XML variant is four times the size of the MT one and is not boxed. One of
/// these exists per worker per open file, so the difference is 400-odd bytes of
/// stack once per file, against an indirection on `next_row`, which is called
/// once per party.
#[allow(clippy::large_enum_variant)]
pub enum Addresses<R: BufRead> {
    Xml(AddressStream<R>),
    Mt(MtAddressStream<R>),
}

impl<R: BufRead> Addresses<R> {
    pub fn new(reader: R, source: &str, mt: bool) -> Self {
        match mt {
            true => Addresses::Mt(MtAddressStream::new(reader, source)),
            false => Addresses::Xml(AddressStream::new(reader, source)),
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AddrRow>, Box<dyn Error>> {
        match self {
            Addresses::Xml(stream) => stream.next_row(),
            Addresses::Mt(stream) => stream.next_row(),
        }
    }
}

/// Which party field a tag is, and whether it names a customer or a bank.
///
/// Fields 50 to 59 are the party fields of every payment type: 50 and 59 are the
/// customers at each end, and everything between them is an institution on the
/// route. The names are the ones the MT readers already use for their columns,
/// so a caller reading `read_mt101` and `audit_addresses` side by side sees one
/// vocabulary.
///
/// Field 50 is two fields sharing a number. Options C and L name whoever
/// instructed the payment and F, G, H and K name the customer whose account pays
/// it, which is why the option letter is read here and not just the number.
///
/// The number of the message decides the rest, because a direct debit runs the
/// other way: in an MT103 or an MT101 field 59 is the party being paid, and in an
/// MT104 or an MT107 it is the party being debited. Same tag, opposite end of the
/// payment. Message types this does not name fall back to the credit-transfer
/// reading, which is what fields 50 to 59 mean in every type quackiso reads.
fn mt_role(number: Option<&str>, tag: &str) -> Option<(&'static str, &'static str)> {
    let debit = matches!(number, Some("104") | Some("107"));
    let role = match (tag.get(..2)?, tag.as_bytes().get(2)) {
        ("50", Some(b'C' | b'L')) => ("InstructingParty", "PARTY"),
        ("50", _) if debit => ("Creditor", "PARTY"),
        ("50", _) => ("OrderingCustomer", "PARTY"),
        ("51", _) => ("SendingInstitution", "AGENT"),
        ("52", _) if debit => ("CreditorBank", "AGENT"),
        ("52", _) => ("OrderingInstitution", "AGENT"),
        ("53", _) => ("SendersCorrespondent", "AGENT"),
        ("54", _) => ("ReceiversCorrespondent", "AGENT"),
        ("55", _) => ("ThirdReimbursementInstitution", "AGENT"),
        ("56", _) => ("IntermediaryInstitution", "AGENT"),
        ("57", _) if debit => ("DebtorBank", "AGENT"),
        ("57", _) => ("AccountWithInstitution", "AGENT"),
        ("58", _) => ("BeneficiaryInstitution", "AGENT"),
        ("59", _) if debit => ("Debtor", "PARTY"),
        ("59", _) => ("Beneficiary", "PARTY"),
        _ => return None,
    };
    Some(role)
}

/// The same audit over SWIFT MT, one message at a time.
///
/// Peak memory is one message plus its parties, which is the bound the MT
/// readers already work to: a statement is one message the way an `<Ntry>`
/// subtree is the bounded unit of camt.053.
pub struct MtAddressStream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    queue: VecDeque<AddrRow>,
    saw_message: bool,
}

impl<R: BufRead> MtAddressStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        MtAddressStream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            queue: VecDeque::new(),
            saw_message: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<AddrRow>, Box<dyn Error>> {
        loop {
            if let Some(row) = self.queue.pop_front() {
                return Ok(Some(row));
            }
            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_message {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no SWIFT MT message found — is this a SWIFT MT file?",
                        self.source
                    )
                    .into())
                };
            };
            self.saw_message = true;
            self.load(&msg);
        }
    }

    /// Every party field of one message, queued in the order the message states
    /// them.
    fn load(&mut self, msg: &str) {
        let body = mt::block(msg, 4).unwrap_or(msg);
        let fields = mt::Fields::parse(body);
        let number = mt::message_number(msg, &fields);
        let family = number.as_deref().map(|number| format!("mt.{number}"));
        let message_id = fields.value("20");

        // How many times each tag has been seen, so a repeated field says which
        // occurrence it is. An MT202COV states `:52a:` in both of its sequences
        // and an MT101 repeats its whole transaction sequence, so without this
        // two rows of one message would be indistinguishable.
        let mut seen: Vec<(&str, usize)> = Vec::new();
        for field in fields.iter() {
            let Some((role, kind)) = mt_role(number.as_deref(), field.tag) else {
                continue;
            };
            let count = match seen.iter_mut().find(|(tag, _)| *tag == field.tag) {
                Some(entry) => {
                    entry.1 += 1;
                    entry.1
                }
                None => {
                    seen.push((field.tag, 1));
                    1
                }
            };
            let party_path = match count {
                1 => field.tag.to_string(),
                n => format!("{}#{n}", field.tag),
            };
            self.queue.push_back(row(
                mt_party(field.tag, &field.value, role, kind, party_path),
                family.as_deref(),
                message_id,
                &self.source,
            ));
        }
    }
}

/// One MT party field as the thing the classifier grades.
///
/// The mapping that matters is which MT line becomes which MX count. A free-text
/// line of a `:50K:` is an `<AdrLine>`: unlabelled, and the reason the mandate
/// exists. A `2/` of a `:50F:` is the same thing numbered, so it counts as a line
/// too -- the hybrid limit of two applies to it exactly as it does in XML. Only
/// `3/` states the town and the country where something other than a human can
/// find them, so only `3/` counts as structured.
fn mt_party(
    tag: &str,
    value: &str,
    role: &'static str,
    kind: &'static str,
    party_path: String,
) -> Party {
    let field = mt::party_field(tag, value);
    let identifies = mt::identifies(tag);
    Party {
        depth: 0,
        role: role.to_string(),
        kind,
        party_path,
        // MT states no transaction ordinal this can read without knowing each
        // type's sequences, and `party_path` locates the party instead.
        record_index: None,
        name: (identifies == mt::Identifies::Name)
            .then(|| field.identifier.map(str::to_string))
            .flatten(),
        bic: (identifies == mt::Identifies::Bic)
            .then(|| field.identifier.map(str::to_string))
            .flatten(),
        town: field.town.map(str::to_string),
        country: field.country.map(str::to_string),
        structured: field.structured(),
        line_text: field.lines.iter().map(|l| l.to_string()).collect(),
    }
}

/// What one XML event is to this walk, decoded before the walk touches its own
/// state: an event borrows the read buffer, and the buffer lives in that state.
enum Act {
    Eof,
    /// An element start, self-closing or not, with the family its own namespace
    /// declaration names when it carries one.
    Element(String, Option<String>),
    Pop,
    Text(String),
    None,
}

/// Whether this element opens a party or an agent, and which.
fn role_kind(name: &str) -> Option<&'static str> {
    if PARTY_ROLES.contains(&name) {
        Some("PARTY")
    } else if AGENT_ROLES.contains(&name) {
        Some("AGENT")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    pub(super) fn rows(xml: &str) -> Vec<AddrRow> {
        let mut stream = AddressStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            out.push(row);
        }
        out
    }

    /// The same, over SWIFT MT.
    fn mt_rows(text: &str) -> Vec<AddrRow> {
        let mut stream = MtAddressStream::new(Cursor::new(text.as_bytes()), "test.fin");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            out.push(row);
        }
        out
    }

    /// One MT103 around a party field.
    fn mt103(party: &str) -> String {
        format!(
            "{{1:F01FOODESMMAXXX0000000000}}{{2:I103BICFOOYYXXXXN}}{{4:\n\
             :20:REF-1\n:23B:CRED\n:32A:260730EUR1000,00\n{party}\n:71A:SHA\n-}}"
        )
    }

    /// A party wrapped in the smallest pacs.008 that carries one.
    fn pacs008(party: &str) -> String {
        format!(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pacs.008.001.08\">\
             <FIToFICstmrCdtTrf><GrpHdr><MsgId>M1</MsgId></GrpHdr>\
             <CdtTrfTxInf>{party}</CdtTrfTxInf></FIToFICstmrCdtTrf></Document>"
        )
    }

    #[test]
    fn town_and_country_in_their_own_elements_with_no_address_line_is_structured() {
        let rows = rows(&pacs008(
            "<Cdtr><Nm>ACME</Nm><PstlAdr><StrtNm>High St</StrtNm><BldgNb>1</BldgNb>\
             <PstCd>EC1</PstCd><TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.address_format.as_deref(), Some("STRUCTURED"));
        assert_eq!(row.town.as_deref(), Some("London"));
        assert_eq!(row.country.as_deref(), Some("GB"));
        assert_eq!(row.structured_elements, Some(5));
        assert_eq!(row.address_lines, Some(0));
        assert_eq!(row.finding, None, "a structured address is compliant");
    }

    #[test]
    fn address_lines_beside_town_and_country_are_hybrid() {
        let rows = rows(&pacs008(
            "<Cdtr><Nm>ACME</Nm><PstlAdr><AdrLine>1 High St</AdrLine>\
             <TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("HYBRID"));
        assert_eq!(rows[0].address_lines, Some(1));
        assert_eq!(
            rows[0].finding, None,
            "one short line is within the hybrid form"
        );
    }

    #[test]
    fn address_lines_alone_are_unstructured_and_named_as_the_reason() {
        let rows = rows(&pacs008(
            "<Cdtr><Nm>ACME</Nm><PstlAdr><AdrLine>1 High St</AdrLine>\
             <AdrLine>London GB</AdrLine></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("UNSTRUCTURED"));
        assert_eq!(
            rows[0].finding.as_deref(),
            Some("address lines with no TwnNm and no Ctry")
        );
    }

    /// The trap the mandate is aimed at: the town and country are written, but
    /// inside a free-text line, so the message is not compliant.
    #[test]
    fn a_town_inside_an_address_line_does_not_count_as_a_town() {
        let rows = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>1 High St</AdrLine>\
             <AdrLine>LONDON, GB</AdrLine></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].town, None);
        assert_eq!(rows[0].country, None);
        assert!(rows[0].finding.is_some());
    }

    #[test]
    fn a_hybrid_address_may_hold_two_lines_and_no_more() {
        let two = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>a</AdrLine><AdrLine>b</AdrLine>\
             <TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(two[0].finding, None);
        let three = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>a</AdrLine><AdrLine>b</AdrLine><AdrLine>c</AdrLine>\
             <TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(
            three[0].finding.as_deref(),
            Some("3 address lines, at most 2 permitted")
        );
    }

    /// A blank line and a self-closing one are still elements on the wire, and
    /// the network counts elements. Counting values instead let a three-line
    /// address with one blank line pass a limit of two.
    #[test]
    fn a_blank_address_line_is_still_a_line() {
        let rows = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>a</AdrLine><AdrLine>   </AdrLine><AdrLine/>\
             <TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_lines, Some(3));
        assert_eq!(
            rows[0].longest_address_line,
            Some(1),
            "only 'a' has content"
        );
        assert_eq!(
            rows[0].finding.as_deref(),
            Some("3 address lines, at most 2 permitted")
        );
    }

    /// What the counts cannot say. Both of these are refused for the same stated
    /// reason, and the work behind them is not the same: one needs its town
    /// labelled, the other needs a town. Real traffic holds both -- prowide's
    /// corpus has `FOOSTREET 65 / MADRID SPAIN` beside a bare `BEX 99` -- so the
    /// audit hands over the lines and leaves the judgement to whoever reads them.
    #[test]
    fn address_text_carries_the_lines_the_refusal_is_about() {
        let labelled = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>FOOSTREET 65</AdrLine>\
             <AdrLine>MADRID SPAIN 28010</AdrLine></PstlAdr></Cdtr>",
        ));
        let bare = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>BEX 99</AdrLine></PstlAdr></Cdtr>",
        ));
        assert_eq!(
            labelled[0].address_text.as_deref(),
            Some("FOOSTREET 65\nMADRID SPAIN 28010")
        );
        assert_eq!(bare[0].address_text.as_deref(), Some("BEX 99"));
        assert_eq!(labelled[0].finding, bare[0].finding, "same stated reason");
    }

    /// The text and the count are read off one list, so a blank line cannot make
    /// them disagree: three lines, three entries, two of them empty.
    #[test]
    fn a_blank_line_holds_its_place_in_the_address_text() {
        let rows = rows(&pacs008(
            "<Cdtr><PstlAdr><AdrLine>a</AdrLine><AdrLine>   </AdrLine><AdrLine/>\
             </PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_text.as_deref(), Some("a\n\n"));
        assert_eq!(rows[0].address_lines, Some(3));
    }

    /// No line, no text: a structured address has nothing free to hand over.
    #[test]
    fn a_structured_address_has_no_address_text() {
        let rows = rows(&pacs008(
            "<Cdtr><PstlAdr><TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("STRUCTURED"));
        assert_eq!(rows[0].address_text, None);
    }

    /// The MT side hands over the same evidence: the free-text lines under the
    /// party field, which is where an MT address lives when it is not option F.
    #[test]
    fn mt_address_text_is_the_free_text_lines() {
        let rows = mt_rows(&mt103(":59:/12345\nJOHN SMITH\n1 HIGH STREET\nLONDON GB"));
        assert_eq!(
            rows[0].address_text.as_deref(),
            Some("1 HIGH STREET\nLONDON GB"),
            "the name is not an address line"
        );
        assert_eq!(rows[0].address_lines, Some(2));
    }

    /// A party whose whole address is blank elements has no address: `NONE` and
    /// no finding, because there is nothing on the wire to refuse.
    #[test]
    fn an_empty_postal_address_is_no_address() {
        let rows = rows(&pacs008(
            "<Cdtr><Nm>Blank</Nm><PstlAdr><TwnNm/><Ctry/></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("NONE"));
        assert_eq!(rows[0].structured_elements, Some(0));
        assert_eq!(rows[0].finding, None);
    }

    /// Two complete messages in one envelope. Identity and transaction numbering
    /// belong to a message and not to a file: ADR 0007 names the same trap on the
    /// pacs.028 side, where a per-file latch read the second message as part of
    /// the first.
    ///
    /// The two messages also state their identity differently, which is why the
    /// leaf is `sniff::is_message_id` and not any element named `MsgId`: a
    /// pacs.008 says `GrpHdr/MsgId`, and camt.056 has no group header at all and
    /// says `Assgnmt/Id`.
    #[test]
    fn each_message_of_an_envelope_carries_its_own_identity_and_numbering() {
        let rows = rows(
            "<BizMsgEnvlp>\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pacs.008.001.08\">\
             <FIToFICstmrCdtTrf><GrpHdr><MsgId>MSG-ONE</MsgId></GrpHdr>\
             <CdtTrfTxInf><Cdtr><Nm>First</Nm></Cdtr></CdtTrfTxInf>\
             </FIToFICstmrCdtTrf></Document>\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.056.001.08\">\
             <FIToFIPmtCxlReq><Assgnmt><Id>CXL-ASGN-2</Id></Assgnmt>\
             <Undrlyg><TxInf><Cdtr><Nm>Second</Nm></Cdtr>\
             <OrgnlTxRef><Dbtr><Nm>Original payer</Nm></Dbtr></OrgnlTxRef>\
             </TxInf></Undrlyg></FIToFIPmtCxlReq></Document></BizMsgEnvlp>",
        );
        let seen: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.name.as_deref().unwrap(),
                    r.family.as_deref().unwrap(),
                    r.message_id.as_deref().unwrap(),
                    r.record_index,
                    r.party_path.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("First", "pacs.008", "MSG-ONE", Some(1), "CdtTrfTxInf/Cdtr"),
                ("Second", "camt.056", "CXL-ASGN-2", Some(1), "TxInf/Cdtr"),
                // A party the cancellation copies out of the payment it cancels.
                // It is audited like any other, and `party_path` is what says it
                // describes the original rather than this message.
                (
                    "Original payer",
                    "camt.056",
                    "CXL-ASGN-2",
                    Some(1),
                    "TxInf/OrgnlTxRef/Dbtr"
                ),
            ],
            "the second message is not a continuation of the first"
        );
    }

    /// The same, on a file the corpus already had: two status reports in one
    /// `<Document>`, which is the shape that made this a defect rather than a
    /// hypothesis.
    #[test]
    fn two_reports_in_one_document_are_two_messages() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pacs002_two_reports.xml");
        let file = std::fs::File::open(&path).expect("the fixture opens");
        let mut stream = AddressStream::new(std::io::BufReader::new(file), "two.xml");
        let mut ids = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            ids.push((row.message_id.unwrap_or_default(), row.record_index));
        }
        assert_eq!(
            ids,
            [
                ("STS-A-260730".to_string(), None),
                ("STS-A-260730".to_string(), None),
                ("STS-A-260730".to_string(), Some(1)),
                ("STS-A-260730".to_string(), Some(1)),
                ("STS-B-260730".to_string(), None),
                ("STS-B-260730".to_string(), None),
            ],
            "each report's agents belong to that report"
        );
    }

    #[test]
    fn an_address_line_past_seventy_characters_is_a_finding_and_is_measured_in_characters() {
        let long = "ä".repeat(71);
        let rows = rows(&pacs008(&format!(
            "<Cdtr><PstlAdr><AdrLine>{long}</AdrLine>\
             <TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>"
        )));
        assert_eq!(
            rows[0].longest_address_line,
            Some(71),
            "characters, not bytes"
        );
        assert_eq!(
            rows[0].finding.as_deref(),
            Some("an address line of 71 characters, at most 70 permitted")
        );
    }

    #[test]
    fn a_structured_address_missing_its_town_is_a_finding() {
        let rows = rows(&pacs008(
            "<Cdtr><PstlAdr><StrtNm>High St</StrtNm><Ctry>GB</Ctry></PstlAdr></Cdtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("STRUCTURED"));
        assert_eq!(rows[0].finding.as_deref(), Some("no TwnNm"));
    }

    #[test]
    fn an_agent_named_by_bic_alone_carries_no_address_and_no_finding() {
        let rows = rows(&pacs008(
            "<CdtrAgt><FinInstnId><BICFI>DEUTDEFF</BICFI></FinInstnId></CdtrAgt>",
        ));
        assert_eq!(rows[0].party_kind.as_deref(), Some("AGENT"));
        assert_eq!(rows[0].bic.as_deref(), Some("DEUTDEFF"));
        assert_eq!(rows[0].address_format.as_deref(), Some("NONE"));
        assert_eq!(rows[0].finding, None);
    }

    #[test]
    fn a_country_of_residence_outside_pstladr_is_not_an_address() {
        let rows = rows(&pacs008(
            "<Dbtr><Nm>ACME</Nm><CtryOfRes>GB</CtryOfRes></Dbtr>",
        ));
        assert_eq!(rows[0].address_format.as_deref(), Some("NONE"));
        assert_eq!(rows[0].country, None);
    }

    #[test]
    fn every_party_of_a_transaction_is_a_row_and_each_names_where_it_sat() {
        let rows = rows(&pacs008(
            "<Dbtr><Nm>D</Nm></Dbtr><DbtrAgt><FinInstnId><BICFI>AAAABBCC</BICFI>\
             </FinInstnId></DbtrAgt><Cdtr><Nm>C</Nm></Cdtr>",
        ));
        let roles: Vec<_> = rows.iter().filter_map(|r| r.role.as_deref()).collect();
        assert_eq!(roles, ["Dbtr", "DbtrAgt", "Cdtr"]);
        assert!(rows
            .iter()
            .all(|r| r.party_path.as_deref().unwrap().starts_with("CdtTrfTxInf/")));
        assert!(rows.iter().all(|r| r.record_index == Some(1)));
        assert!(rows.iter().all(|r| r.family.as_deref() == Some("pacs.008")));
        assert!(rows.iter().all(|r| r.message_id.as_deref() == Some("M1")));
    }

    #[test]
    fn transactions_are_numbered_and_a_group_level_party_belongs_to_none_of_them() {
        let rows = rows(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\">\
             <CstmrCdtTrfInitn><GrpHdr><MsgId>M9</MsgId><InitgPty><Nm>P</Nm></InitgPty></GrpHdr>\
             <PmtInf><Dbtr><Nm>D</Nm></Dbtr>\
             <CdtTrfTxInf><Cdtr><Nm>C1</Nm></Cdtr></CdtTrfTxInf>\
             <CdtTrfTxInf><Cdtr><Nm>C2</Nm></Cdtr></CdtTrfTxInf></PmtInf>\
             </CstmrCdtTrfInitn></Document>",
        );
        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.role.as_deref().unwrap(), r.record_index))
            .collect();
        assert_eq!(
            seen,
            [
                ("InitgPty", None),
                ("Dbtr", None),
                ("Cdtr", Some(1)),
                ("Cdtr", Some(2)),
            ]
        );
    }

    /// The cash-management families are outside the mandate, so their parties
    /// are reported with their format and never with a finding.
    #[test]
    fn an_out_of_scope_family_reports_the_format_and_raises_no_finding() {
        let rows = rows(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>S1</MsgId></GrpHdr><Stmt><Ntry><NtryDtls><TxDtls>\
             <RltdPties><Dbtr><Nm>D</Nm><PstlAdr><AdrLine>somewhere</AdrLine></PstlAdr></Dbtr>\
             </RltdPties></TxDtls></NtryDtls></Ntry></Stmt></BkToCstmrStmt></Document>",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].family.as_deref(), Some("camt.053"));
        assert_eq!(rows[0].address_format.as_deref(), Some("UNSTRUCTURED"));
        assert_eq!(
            rows[0].finding, None,
            "camt.053 is excluded from the address mandate"
        );
    }

    #[test]
    fn a_file_that_is_not_an_iso_message_fails_rather_than_returning_nothing() {
        let mut stream = AddressStream::new(Cursor::new(b"<html><body/></html>"), "page.html");
        let err = stream.next_row().expect_err("a non-ISO file is an error");
        assert!(err.to_string().contains("no ISO 20022 message found"));
    }

    #[test]
    fn a_message_with_no_parties_in_it_is_zero_rows_and_not_an_error() {
        let rows = rows(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pacs.008.001.08\">\
             <FIToFICstmrCdtTrf><GrpHdr><MsgId>M1</MsgId></GrpHdr></FIToFICstmrCdtTrf></Document>",
        );
        assert!(rows.is_empty());
    }

    /// The committed fixture, which holds all four shapes and three of the
    /// findings in one message. Read from disk rather than from a string
    /// literal, so the indentation and line endings a real file has are in the
    /// path this walks.
    #[test]
    fn the_four_shapes_of_one_real_message() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pacs008_address_formats.xml");
        let file = std::fs::File::open(&path).expect("the fixture opens");
        let mut stream =
            AddressStream::new(std::io::BufReader::new(file), &path.display().to_string());
        let mut rows = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            rows.push(row);
        }

        let seen: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.role.as_deref().unwrap(),
                    r.address_format.as_deref().unwrap(),
                    r.finding.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("Dbtr", "STRUCTURED", None),
                (
                    "DbtrAgt",
                    "HYBRID",
                    Some("an address line of 73 characters, at most 70 permitted")
                ),
                ("Cdtr", "HYBRID", None),
                (
                    "UltmtCdtr",
                    "UNSTRUCTURED",
                    Some("address lines with no TwnNm")
                ),
                ("CdtrAgt", "NONE", None),
            ]
        );

        // The identity every row carries, so a folder of files stays attributable.
        assert!(rows.iter().all(|r| r.family.as_deref() == Some("pacs.008")));
        assert!(rows
            .iter()
            .all(|r| r.message_id.as_deref() == Some("ADDR-FORMATS-1")));
        assert!(rows.iter().all(|r| r.record_index == Some(1)));
        assert!(rows
            .iter()
            .all(|r| r.source_file.as_deref() == Some(path.display().to_string().as_str())));

        // An agent's own name and BIC are read even when they sit under
        // FinInstnId beside the address.
        let agent = &rows[1];
        assert_eq!(agent.party_kind.as_deref(), Some("AGENT"));
        assert_eq!(agent.bic.as_deref(), Some("ABNANL2A"));
        assert_eq!(agent.name.as_deref(), Some("ABN AMRO"));
        assert_eq!(agent.town.as_deref(), Some("Amsterdam"));
        assert_eq!(agent.address_lines, Some(2));
        assert_eq!(agent.longest_address_line, Some(73));
    }

    /// Real bytes, from prowide-core's MT103-out-ack.rje. Lagos and Nigeria are
    /// both in there and no element says so, which is the whole finding.
    #[test]
    fn an_mt_name_and_address_is_unstructured_and_says_why() {
        let rows = mt_rows(&mt103(
            ":50K:/22222222222\nOLD MUTUAL GENERAL INSURAN\n226 AWOLOWO WAY 322\nLAGOS NIGERIA",
        ));
        assert_eq!(rows.len(), 1);
        let party = &rows[0];
        assert_eq!(party.family.as_deref(), Some("mt.103"));
        assert_eq!(party.message_id.as_deref(), Some("REF-1"));
        assert_eq!(party.party_path.as_deref(), Some("50K"));
        assert_eq!(party.role.as_deref(), Some("OrderingCustomer"));
        assert_eq!(party.party_kind.as_deref(), Some("PARTY"));
        assert_eq!(party.name.as_deref(), Some("OLD MUTUAL GENERAL INSURAN"));
        assert_eq!(
            (party.town.as_deref(), party.country.as_deref()),
            (None, None)
        );
        assert_eq!(party.address_lines, Some(2));
        assert_eq!(party.structured_elements, Some(0));
        assert_eq!(party.address_format.as_deref(), Some("UNSTRUCTURED"));
        assert_eq!(
            party.finding.as_deref(),
            Some("address lines with no TwnNm and no Ctry")
        );
    }

    /// The same audit, and the one MT option that survives the date: `3/` states
    /// the country and the town where a translator can find them, so the address
    /// lands as HYBRID and nothing is refused.
    #[test]
    fn an_mt_option_f_address_is_hybrid_and_passes() {
        let rows = mt_rows(&mt103(
            ":50F:/BE30001216371411\n1/JOHN SMITH\n2/HOOGSTRAAT 6\n3/BE/BRUSSELS",
        ));
        let party = &rows[0];
        assert_eq!(party.name.as_deref(), Some("JOHN SMITH"));
        assert_eq!(party.town.as_deref(), Some("BRUSSELS"));
        assert_eq!(party.country.as_deref(), Some("BE"));
        assert_eq!(party.address_lines, Some(1));
        assert_eq!(party.structured_elements, Some(2));
        assert_eq!(party.address_format.as_deref(), Some("HYBRID"));
        assert_eq!(party.finding, None);
    }

    /// An agent named by a BIC needs no address in MT either, and the same three
    /// counts read zero rather than reading the BIC as a name.
    #[test]
    fn an_mt_agent_named_by_bic_carries_no_address() {
        let rows = mt_rows(&mt103(":57A:/98765\nBARCGB22XXX"));
        let agent = &rows[0];
        assert_eq!(agent.party_kind.as_deref(), Some("AGENT"));
        assert_eq!(agent.role.as_deref(), Some("AccountWithInstitution"));
        assert_eq!(agent.bic.as_deref(), Some("BARCGB22XXX"));
        assert_eq!(agent.name, None);
        assert_eq!(agent.address_format.as_deref(), Some("NONE"));
        assert_eq!(agent.finding, None);
    }

    /// A statement carries no party field at all. Zero rows is the answer, not an
    /// error: the file was read and it named nobody.
    #[test]
    fn an_mt_statement_names_no_parties_and_is_not_an_error() {
        let statement = "{1:F01NWBKGB2LAXXX0000000000}{2:I940NWBKGB2LXXXXN}{4:\n\
             :20:ST-1\n:25:GB29NWBK60161331926819\n:28C:1/1\n\
             :60F:C260730EUR1000,00\n:62F:C260730EUR1000,00\n-}";
        assert!(mt_rows(statement).is_empty());
    }

    /// A tag may occur twice in one message: an MT202COV states `:52a:` in the
    /// cover and again in the underlying transfer. Both are rows, and each says
    /// which occurrence it is, because MT numbers its sequences differently in
    /// every type and this walk does not guess at transaction ordinals.
    #[test]
    fn a_repeated_party_field_says_which_occurrence_it_is() {
        let cov = "{1:F01FOODESMMAXXX0000000000}{2:I202BICFOOYYXXXXN}{3:{119:COV}}{4:\n\
             :20:COV-1\n:21:REL-1\n:32A:260730EUR1000,00\n\
             :52A:FOODESMMXXX\n:58A:BICFOOYYXXX\n\
             :50K:/1\nUNDERLYING PAYER\nSOMEWHERE STREET 1\n\
             :52A:DEUTDEFFXXX\n:59:/2\nUNDERLYING PAYEE\n-}";
        let seen: Vec<_> = mt_rows(cov)
            .iter()
            .map(|r| (r.party_path.clone().unwrap(), r.record_index))
            .collect();
        assert_eq!(
            seen,
            [
                ("52A".to_string(), None),
                ("58A".to_string(), None),
                ("50K".to_string(), None),
                ("52A#2".to_string(), None),
                ("59".to_string(), None),
            ]
        );
    }

    /// A file the guard let through as MT that frames no message at all is an
    /// error, the way an XML file with no message in it is.
    #[test]
    fn mt_bytes_with_no_message_in_them_are_an_error() {
        let mut stream = MtAddressStream::new(Cursor::new(b"nothing here".as_slice()), "test.fin");
        let err = stream.next_row().expect_err("no message is an error");
        assert!(err.to_string().contains("no SWIFT MT message found"));
    }
}

#[cfg(test)]
mod header_and_cheque_tests {
    use super::tests::rows;
    use super::*;
    use std::io::Cursor;

    /// A cheque presentment: no namespace, an unknown wrapper, and three
    /// parties spelled the way the cheque messages spell them. Before `Pyer`,
    /// `Pyee` and `DrwrAgt` were roles, this file audited as zero rows - which
    /// reads exactly like a file with nothing wrong in it.
    #[test]
    fn a_cheque_named_only_by_its_header_yields_its_own_three_roles() {
        let file = std::fs::File::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join("envelope_apphdr_camt107.xml"),
        )
        .expect("the fixture opens");
        let mut stream = AddressStream::new(std::io::BufReader::new(file), "cheque.xml");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            out.push((
                row.role.unwrap_or_default(),
                row.party_kind.unwrap_or_default(),
                row.family.unwrap_or_default(),
                row.message_id.unwrap_or_default(),
                row.address_format.unwrap_or_default(),
                row.finding,
            ));
        }
        assert_eq!(
            out,
            [
                (
                    "Pyer".to_string(),
                    "PARTY".to_string(),
                    "camt.107".to_string(),
                    "CHQ-PRESENT-0107".to_string(),
                    "UNSTRUCTURED".to_string(),
                    Some("address lines with no TwnNm".to_string()),
                ),
                (
                    "Pyee".to_string(),
                    "PARTY".to_string(),
                    "camt.107".to_string(),
                    "CHQ-PRESENT-0107".to_string(),
                    "HYBRID".to_string(),
                    None,
                ),
                (
                    "DrwrAgt".to_string(),
                    "AGENT".to_string(),
                    "camt.107".to_string(),
                    "CHQ-PRESENT-0107".to_string(),
                    "NONE".to_string(),
                    None,
                ),
            ],
            "the header names the family and the cheque roles are graded"
        );
    }

    /// Roles the cheque schemas define and the pinned corpus never states are
    /// not roles here. An inferred role is a column no fixture can be wrong
    /// about, and a row nobody asked for beside the three that matter.
    #[test]
    fn a_role_the_corpus_does_not_state_is_not_invented() {
        for absent in ["Drwr", "Drwee", "Endrsee", "ChqDpstr"] {
            assert_eq!(
                role_kind(absent),
                None,
                "{absent} is not in the pinned corpus"
            );
        }
        assert_eq!(role_kind("Pyer"), Some("PARTY"));
        assert_eq!(role_kind("Pyee"), Some("PARTY"));
        assert_eq!(role_kind("DrwrAgt"), Some("AGENT"));
    }

    /// `Rcpt` is now a mapped container, and it is a name any message may use
    /// for something of its own. Inside a message already under way it must
    /// stay an ordinary element: opening a message there would take the
    /// identity and the transaction numbering with it.
    #[test]
    fn a_mapped_name_nested_in_another_message_does_not_open_a_message() {
        let got = rows(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pacs.008.001.08\">\
             <FIToFICstmrCdtTrf><GrpHdr><MsgId>M1</MsgId></GrpHdr>\
             <CdtTrfTxInf><Rcpt><MsgHdr><MsgId>NOT-THE-MESSAGE</MsgId></MsgHdr></Rcpt>\
             <Cdtr><Nm>ACME</Nm><PstlAdr><TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Cdtr>\
             </CdtTrfTxInf></FIToFICstmrCdtTrf></Document>",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].family.as_deref(), Some("pacs.008"));
        assert_eq!(got[0].message_id.as_deref(), Some("M1"));
        assert_eq!(
            got[0].record_index,
            Some(1),
            "the transaction numbering belongs to the pacs.008"
        );
    }

    /// The other side of the same rule: a sibling message after the first one
    /// closes is its own message again.
    #[test]
    fn a_mapped_container_after_a_closed_message_opens_a_new_one() {
        let got = rows(
            "<Envelope><Document>\
             <Rcpt><MsgHdr><MsgId>R-1</MsgId></MsgHdr>\
             <Pyer><Nm>First</Nm><PstlAdr><TwnNm>Bern</TwnNm><Ctry>CH</Ctry></PstlAdr></Pyer>\
             </Rcpt>\
             <ChqPresntmntNtfctn><GrpHdr><MsgId>C-1</MsgId></GrpHdr>\
             <Pyee><Nm>Second</Nm><PstlAdr><TwnNm>Basel</TwnNm><Ctry>CH</Ctry></PstlAdr></Pyee>\
             </ChqPresntmntNtfctn></Document></Envelope>",
        );
        assert_eq!(
            got.iter()
                .map(|r| (
                    r.family.clone().unwrap_or_default(),
                    r.message_id.clone().unwrap_or_default(),
                    r.role.clone().unwrap_or_default()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "camt.025".to_string(),
                    "R-1".to_string(),
                    "Pyer".to_string()
                ),
                (
                    "camt.107".to_string(),
                    "C-1".to_string(),
                    "Pyee".to_string()
                ),
            ]
        );
    }

    /// A Document that nothing named claims no message scope, so the container
    /// underneath a wrapper this does not know still names the family. Claiming
    /// it unconditionally protected an identity that did not exist and left
    /// every row of the file with `family` NULL.
    #[test]
    fn an_unknown_wrapper_does_not_cost_the_message_its_family() {
        let got = rows(
            "<Document><NtlWrapper>\
             <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr>\
             <Stmt><Ntry><NtryDtls><TxDtls><RltdPties><Cdtr><Nm>ACME</Nm>\
             <PstlAdr><TwnNm>Basel</TwnNm><Ctry>CH</Ctry></PstlAdr></Cdtr></RltdPties>\
             </TxDtls></NtryDtls></Ntry></Stmt></BkToCstmrStmt>\
             </NtlWrapper></Document>",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].family.as_deref(), Some("camt.053"));
        assert_eq!(got[0].message_id.as_deref(), Some("M-1"));
    }

    /// A header above the message ranks below a message namespace and above the
    /// container name, exactly as it does in the sniffer.
    #[test]
    fn the_header_family_ranks_between_the_namespace_and_the_container() {
        let namespaced = rows(
            "<Envelope><AppHdr><MsgDefIdr>camt.107.001.01</MsgDefIdr></AppHdr>\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pacs.008.001.08\">\
             <FIToFICstmrCdtTrf><GrpHdr><MsgId>M1</MsgId></GrpHdr>\
             <CdtTrfTxInf><Cdtr><Nm>ACME</Nm><PstlAdr><TwnNm>London</TwnNm><Ctry>GB</Ctry>\
             </PstlAdr></Cdtr></CdtTrfTxInf></FIToFICstmrCdtTrf></Document></Envelope>",
        );
        assert_eq!(namespaced[0].family.as_deref(), Some("pacs.008"));

        let headed = rows(
            "<Envelope><AppHdr><MsgDefIdr>camt.107.001.01</MsgDefIdr></AppHdr>\
             <Document><ChqPresntmntNtfctn><GrpHdr><MsgId>C-1</MsgId></GrpHdr>\
             <Pyee><Nm>ACME</Nm><PstlAdr><TwnNm>London</TwnNm><Ctry>GB</Ctry></PstlAdr></Pyee>\
             </ChqPresntmntNtfctn></Document></Envelope>",
        );
        assert_eq!(headed[0].family.as_deref(), Some("camt.107"));
    }

    /// A header and nothing else is not a message, so the audit refuses the
    /// file the way it refuses any XML that names no message.
    #[test]
    fn a_header_alone_does_not_make_a_file_an_iso_message() {
        let xml = "<BizMsgEnvlp><AppHdr><MsgDefIdr>camt.107.001.01</MsgDefIdr></AppHdr>\
                   </BizMsgEnvlp>";
        let mut stream = AddressStream::new(Cursor::new(xml.as_bytes()), "header.xml");
        let err = match stream.next_row() {
            Ok(Some(_)) => panic!("a header carries no party"),
            Ok(None) => panic!("a header-only file must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("no ISO 20022 message found"), "got {err}");
    }
}
