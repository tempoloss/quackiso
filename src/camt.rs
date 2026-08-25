//! The statement walk the camt.05x readers share: find the message, find the
//! statements inside it, and hand out one record of a chosen kind at a time.
//!
//! Five functions read a camt.052 report, a camt.053 statement or a camt.054
//! notification, at five different grains - an entry, a transaction, a balance,
//! an amount block, a remittance leaf. What is identical across all five is
//! everything above the record: which files are one of those three messages,
//! where the per-account containers are, which statement a record belongs to,
//! and what that statement says about itself. Five copies of that is how four
//! of them come to disagree about which `<Acct>` an entry was under.
//!
//! This is an internal parser primitive and not a table function. It owns:
//!
//! * the outer message gate - camt.052, camt.053 or camt.054, by container
//!   name, by era element name, or by the namespace or `AppHdr/MsgDefIdr` that
//!   identified a `<Document>`;
//! * the inner `Rpt`/`Stmt`/`Ntfctn` gate, matched against the family so a
//!   camt.053 does not report a `<Rpt>` it cannot have;
//! * the context every row repeats, captured only at the exact scoped paths, so
//!   an `<IBAN>` in supplementary data or in the next statement cannot overwrite
//!   it;
//! * `statement_index`, which is source-global and 1-based, and the per-record
//!   index, which restarts inside every statement.
//!
//! [`EntryStream`](crate::stream::EntryStream) is not replaced by it. ADR 0004
//! keeps that reader's looser rule - an `<Ntry>` becomes a row wherever it
//! appears - and this walk emits only direct children of a statement, which is
//! a different promise. The two meet at the scope columns: an entry this walk
//! would emit is exactly an entry whose `statement_index` and `entry_index` are
//! not NULL, and that is what the supplementary readers join on.
//!
//! Peak memory is one record plus this state. The other repeated record of a
//! statement is skipped through [`wire::skip_subtree`], so a scan for balances
//! walks past three million entries in constant space instead of copying each
//! one out to drop it.

use std::error::Error;
use std::io::BufRead;
use std::marker::PhantomData;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::{Balance, Ntry};
use crate::sniff::{
    family_of_container, family_of_identifier, find_identifier, identifier_ns,
    is_message_definition, message_definition,
};
use crate::wire;

/// The three families this walk reads, beside the per-account container each
/// one states. The pairing is what identifies a message element; the container
/// names are not matched back against the family, because `read_iso20022`
/// scopes a `<Rpt>` inside a camt.053 and the four readers join on its columns.
const FAMILIES: [(&str, &str); 3] = [
    ("camt.052", "Rpt"),
    ("camt.053", "Stmt"),
    ("camt.054", "Ntfctn"),
];

/// The per-account containers, whatever family stated them.
fn is_statement(name: &str) -> bool {
    FAMILIES.iter().any(|(_, container)| *container == name)
}

/// The one of the three this family string is, as the static name. An identity
/// resolved from a namespace or a header arrives as a `String`; the gate needs
/// to know whether it is one of these and nothing more.
fn known_family(family: &str) -> Option<&'static str> {
    FAMILIES.iter().find(|(f, _)| *f == family).map(|(f, _)| *f)
}

/// The family a message element announces, when it is one of the three. Both
/// spellings: the container name the readers accept, and the earliest editions'
/// element that *is* the identifier.
fn statement_family(name: &str) -> Option<&'static str> {
    let by_container = family_of_container(name);
    let by_identifier = find_identifier(name)
        .filter(|ident| *ident == name)
        .map(family_of_identifier);
    by_container.or(by_identifier).and_then(known_family)
}

/// What the statement around a record says about itself. Every supplementary
/// row repeats this, so it is captured once per statement and cloned once per
/// record rather than per row.
#[derive(Debug, Default, Clone)]
pub struct StatementContext {
    pub msg_id: Option<String>,
    /// `Stmt`, `Ntfctn` or `Rpt`, exactly as the wire spelled it.
    pub statement_kind: Option<String>,
    /// 1-based across the whole source file, so two statements in one document
    /// are 1 and 2 and a second document's first statement is 3. The join key
    /// the entry row carries.
    pub statement_index: i64,
    pub statement_id: Option<String>,
    pub account_iban: Option<String>,
    pub account_currency: Option<String>,
}

/// A record kind this walk can hand out: the element it is, the sibling record
/// it is not, and how to turn one subtree into it.
pub trait StatementRecord: Sized {
    /// The direct child of a statement this stream turns into rows.
    const TAG: &'static str;
    /// The other repeated record a statement holds. Consumed and dropped, so a
    /// scan of one does not pay for the other.
    const SKIPPED: &'static str;

    fn from_xml(xml: &str) -> Result<Self, Box<dyn Error>>;
}

impl StatementRecord for Ntry {
    const TAG: &'static str = "Ntry";
    const SKIPPED: &'static str = "Bal";

    fn from_xml(xml: &str) -> Result<Self, Box<dyn Error>> {
        Ok(quick_xml::de::from_str(xml)?)
    }
}

impl StatementRecord for Balance {
    const TAG: &'static str = "Bal";
    const SKIPPED: &'static str = "Ntry";

    fn from_xml(xml: &str) -> Result<Self, Box<dyn Error>> {
        Ok(quick_xml::de::from_str(xml)?)
    }
}

pub struct StatementRecordStream<R: BufRead, T: StatementRecord> {
    reader: Reader<R>,
    buf: Vec<u8>,
    /// element local-names from root to cursor. Records are consumed as a
    /// subtree and never pushed here.
    path: Vec<String>,
    source: String,
    ctx: StatementContext,
    /// The family of the message being walked, and `path.len()` at the element
    /// that opened it.
    family: Option<&'static str>,
    message_depth: Option<usize>,
    /// `path.len()` at the active `Rpt`/`Stmt`/`Ntfctn`.
    statement_depth: Option<usize>,
    /// The first identifier-bearing namespace above the message, and the first
    /// `AppHdr/MsgDefIdr` of the file. Either can identify a `<Document>` that
    /// declares nothing itself.
    envelope_family: Option<String>,
    header_family: Option<String>,
    /// Whether any per-account container was seen at all. An account with no
    /// movements is a valid statement, so the wrong-file check is on this and
    /// never on the record count.
    saw_statement: bool,
    statement_index: i64,
    record_index: i64,
    _record: PhantomData<T>,
}

impl<R: BufRead, T: StatementRecord> StatementRecordStream<R, T> {
    pub fn new(reader: R, source: &str) -> Self {
        StatementRecordStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: StatementContext::default(),
            family: None,
            message_depth: None,
            statement_depth: None,
            envelope_family: None,
            header_family: None,
            saw_statement: false,
            statement_index: 0,
            record_index: 0,
            _record: PhantomData,
        }
    }

    /// The statement the last record came out of. Valid until the next call to
    /// [`Self::next_record`].
    pub fn context(&self) -> &StatementContext {
        &self.ctx
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// The next record and its 1-based index inside the active statement, or
    /// None at end of document.
    ///
    /// A file with no `Rpt`/`Stmt`/`Ntfctn` in it at all raises rather than
    /// returning nothing: a camt.052/.053/.054 reader pointed at a pain.001
    /// has been pointed at the wrong file, and zero rows would say the file was
    /// empty. A statement with no records of this kind returns nothing, which
    /// is the truth about it.
    pub fn next_record(&mut self) -> Result<Option<(i64, T)>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match wire::next_event(
                &mut self.reader,
                &mut self.buf,
                &self.path,
                &self.source,
            )? {
                Event::Eof => Act::Eof,
                // Decided below rather than here: `at_record` reads `self`, and
                // the event still borrows the read buffer that lives in it.
                Event::Start(e) => Act::Start(
                    wire::local(e.name().as_ref()).into_owned(),
                    identifier_ns(&e),
                ),
                Event::Empty(e) => Act::Empty(
                    wire::local(e.name().as_ref()).into_owned(),
                    identifier_ns(&e),
                ),
                Event::End(_) => Act::Pop,
                ev => match wire::event_text(&ev)? {
                    Some(text) => Act::Text(text),
                    None => Act::None,
                },
            };

            match action {
                Act::Eof => {
                    return match self.saw_statement {
                        true => Ok(None),
                        false => Err(format!(
                            "{}: no <Stmt>, <Ntfctn> or <Rpt> found - is this a \
                             camt.053/.054/.052 message?",
                            self.source
                        )
                        .into()),
                    }
                }
                Act::Start(name, ns) => {
                    if name == T::TAG && self.at_record() {
                        let xml = wire::record_subtree(
                            &mut self.reader,
                            &mut self.buf,
                            T::TAG,
                            &self.source,
                        )?;
                        self.record_index += 1;
                        return Ok(Some((self.record_index, T::from_xml(&xml)?)));
                    }
                    // The other repeated record of a statement, consumed whole.
                    // A balance scan walking three million entries out to drop
                    // them would put the largest subtree back into the bound.
                    if name == T::SKIPPED {
                        wire::skip_subtree(
                            &mut self.reader,
                            &mut self.buf,
                            T::SKIPPED,
                            &self.source,
                        )?;
                        continue;
                    }
                    self.open(&name, ns);
                    self.path.push(name);
                }
                // A self-closing element has no matching End, so it never enters
                // the path and cannot open a scope; the binding it declares is
                // still a binding. It can still be a record: `<Bal/>` states a
                // balance with nothing in it, the same as `<Bal></Bal>`, and one
                // spelling numbering differently from the other would make the
                // index depend on how the writer closed its tags.
                Act::Empty(name, ns) => {
                    if name == T::TAG && self.at_record() {
                        self.record_index += 1;
                        let xml = format!("<{}/>", T::TAG);
                        return Ok(Some((self.record_index, T::from_xml(&xml)?)));
                    }
                    self.remember_namespace(ns);
                }
                Act::Pop => {
                    self.path.pop();
                    if self.statement_depth == Some(self.path.len()) {
                        self.statement_depth = None;
                    }
                    if self.message_depth == Some(self.path.len()) {
                        self.message_depth = None;
                        self.family = None;
                        self.ctx.msg_id = None;
                    }
                }
                Act::Text(text) => self.capture(&text),
                Act::None => {}
            }
        }
    }

    /// Whether the cursor is at a direct child of the active statement. Only
    /// those are records: an `<Ntry>` inside another entry's supplementary data
    /// is not a movement of this account, and a `<Bal>` inside one is not a
    /// position of it.
    fn at_record(&self) -> bool {
        self.statement_depth
            .is_some_and(|depth| self.path.len() == depth + 1)
    }

    /// One element start: the namespace it may declare, the message it may
    /// open, and the statement it may open.
    fn open(&mut self, name: &str, ns: Option<String>) {
        let own = ns
            .as_deref()
            .and_then(find_identifier)
            .map(|ident| family_of_identifier(ident).to_string());
        let mut opened_message = false;
        if self.message_depth.is_none() {
            // A container names the family itself. A `<Document>` takes its own
            // binding first, then the one an envelope above it declared, then
            // the header that named the payload - the sniffer's precedence, so
            // one file cannot be two families depending on who asked.
            let family = match name {
                "Document" => {
                    let candidate = own
                        .clone()
                        .or_else(|| self.envelope_family.clone())
                        .or_else(|| self.header_family.clone());
                    candidate.as_deref().and_then(known_family)
                }
                _ => statement_family(name),
            };
            if let Some(family) = family {
                self.family = Some(family);
                self.message_depth = Some(self.path.len());
                self.ctx.msg_id = None;
                opened_message = true;
            }
        }
        // An envelope's binding is the one above a message, so the element that
        // opens the message is not one: a `<Document>` whose own namespace was
        // taken as its family would otherwise hand that family to the next
        // `<Document>` in the file, which declared none and is not the same
        // message.
        if self.envelope_family.is_none() && !opened_message {
            self.envelope_family = own;
        }
        // The container rule is `EntryStream`'s, deliberately unqualified: any
        // `Stmt`, `Ntfctn` or `Rpt` is this account's container wherever it
        // sits, whether or not a family was resolved and whether or not the
        // family that was resolved spells its container that way. The four
        // readers join on `read_iso20022`'s scope columns, so anything narrower
        // makes the join a lie - a national file with no namespace, and a
        // camt.053 that states `<Rpt>`, are both rows there.
        if self.statement_depth.is_none() && is_statement(name) {
            self.saw_statement = true;
            self.statement_depth = Some(self.path.len());
            self.statement_index += 1;
            self.record_index = 0;
            self.ctx.statement_kind = Some(name.to_string());
            self.ctx.statement_index = self.statement_index;
            self.ctx.statement_id = None;
            self.ctx.account_iban = None;
            self.ctx.account_currency = None;
        }
    }

    /// The first identifier-bearing namespace of the file, for a `<Document>`
    /// that declares none of its own. A self-closing element cannot open a
    /// scope, but the binding it declares is still a binding, so that path
    /// comes through here too. `head.001` never arrives: `identifier_ns` passes
    /// an envelope header's own binding over.
    fn remember_namespace(&mut self, ns: Option<String>) {
        if self.envelope_family.is_some() {
            return;
        }
        self.envelope_family = ns
            .as_deref()
            .and_then(find_identifier)
            .map(|ident| family_of_identifier(ident).to_string());
    }

    /// The context leaves, at the exact paths that own them.
    ///
    /// Scoped rather than matched by tail: a statement's `<Id>` and a
    /// transaction's `<Id>` are the same two characters, and an `<Acct>` inside
    /// `<RltdAcct>` or inside supplementary data is not the account this
    /// statement is about. The record subtrees never enter `path`, so what is
    /// left to get wrong is exactly this.
    fn capture(&mut self, text: &str) {
        if let Some(depth) = self.statement_depth {
            let under = |tail: &[&str]| {
                self.path.len() == depth + 1 + tail.len()
                    && self.path[depth + 1..].iter().zip(tail).all(|(a, b)| a == b)
            };
            if under(&["Id"]) {
                self.ctx.statement_id = Some(text.to_string());
            } else if under(&["Acct", "Ccy"]) {
                self.ctx.account_currency = Some(text.to_string());
            } else if under(&["Acct", "Id", "IBAN"]) {
                self.ctx.account_iban = Some(text.to_string());
            } else if under(&["Acct", "Id", "Othr", "Id"]) {
                // an in-house or custody account has no IBAN to lose to
                self.ctx
                    .account_iban
                    .get_or_insert_with(|| text.to_string());
            }
            return;
        }
        // Above any statement: the group header of the message, and the header
        // of the envelope around it. The header is read wherever it sits,
        // including before the message begins.
        if self.header_family.is_none() && is_message_definition(&self.path) {
            self.header_family =
                message_definition(text).map(|ident| family_of_identifier(ident).to_string());
        }
        // Neither guarded on absence nor on a resolved message: one `<Document>`
        // can hold two complete messages, each with its own `GrpHdr`, and a
        // header can name a message nothing else identified. `EntryStream`
        // assigns on the same path with the same unconditional rule, so the two
        // functions cannot disagree about which message an entry belongs to.
        if wire::ends_with(&self.path, &["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Start(String, Option<String>),
    Empty(String, Option<String>),
    Pop,
    Text(String),
    None,
}

#[cfg(test)]
mod tests {
    use crate::camt_amount_details::AmountDetailStream;
    use crate::camt_balances::BalanceStream;
    use crate::camt_remittance::RemittanceStream;
    use crate::camt_transactions::TransactionStream;
    use std::io::Cursor;

    /// A camt.053 around `stmt`, bound to its own schema.
    fn camt053(stmt: &str) -> String {
        format!(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr>{stmt}\
             </BkToCstmrStmt></Document>"
        )
    }

    /// One balance row, as the facts a row would carry.
    type Bal = (i64, i64, Option<String>, Option<i128>);

    /// Every balance of `xml`, or the refusal it raised.
    fn balances(xml: &str) -> Result<Vec<Bal>, String> {
        let mut stream = BalanceStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut out = Vec::new();
        loop {
            match stream.next_row() {
                Ok(Some(row)) => out.push((
                    row.statement_index.unwrap_or_default(),
                    row.balance_index.unwrap_or_default(),
                    row.balance_type,
                    row.amount,
                )),
                Ok(None) => return Ok(out),
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    fn transactions(xml: &str) -> Result<Vec<(i64, i64, i64, i64)>, String> {
        let mut stream = TransactionStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut out = Vec::new();
        loop {
            match stream.next_row() {
                Ok(Some(row)) => out.push((
                    row.statement_index.unwrap_or_default(),
                    row.entry_index.unwrap_or_default(),
                    row.entry_details_index.unwrap_or_default(),
                    row.transaction_index.unwrap_or_default(),
                )),
                Ok(None) => return Ok(out),
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// The message each balance row names, in row order.
    fn msg_ids(xml: &str) -> Result<Vec<String>, String> {
        let mut stream = BalanceStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut out = Vec::new();
        loop {
            match stream.next_row() {
                Ok(Some(row)) => out.push(row.msg_id.unwrap_or_default()),
                Ok(None) => return Ok(out),
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Every one of the four supplementary readers over one file, as the error
    /// each raised or the number of rows it produced. Their refusals are shared,
    /// so they are asserted together.
    fn all_four(xml: &str) -> [Result<usize, String>; 4] {
        macro_rules! count {
            ($stream:ty) => {{
                let mut stream = <$stream>::new(Cursor::new(xml.as_bytes()), "test.xml");
                let mut rows = 0usize;
                loop {
                    match stream.next_row() {
                        Ok(Some(_)) => rows += 1,
                        Ok(None) => break Ok(rows),
                        Err(e) => break Err(e.to_string()),
                    }
                }
            }};
        }
        [
            count!(TransactionStream<Cursor<&[u8]>>),
            count!(BalanceStream<Cursor<&[u8]>>),
            count!(AmountDetailStream<Cursor<&[u8]>>),
            count!(RemittanceStream<Cursor<&[u8]>>),
        ]
    }

    const ONE_ENTRY: &str = "<Ntry><NtryRef>N-1</NtryRef><Amt Ccy=\"CHF\">1.00</Amt>\
                             <AmtDtls><TxAmt><Amt Ccy=\"CHF\">1.00</Amt></TxAmt></AmtDtls>\
                             <NtryDtls><TxDtls><Amt Ccy=\"CHF\">1.00</Amt>\
                             <RmtInf><Ustrd>Invoice</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>";

    /// All four readers share one walk, so they share one answer about a file
    /// that is not a statement at all.
    #[test]
    fn camt_the_wrong_family_is_refused_by_every_reader_alike() {
        let pain = "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.03\">\
                    <CstmrCdtTrfInitn><GrpHdr><MsgId>P-1</MsgId></GrpHdr>\
                    <PmtInf><CdtTrfTxInf><Amt><InstdAmt Ccy=\"EUR\">1.00</InstdAmt></Amt>\
                    </CdtTrfTxInf></PmtInf></CstmrCdtTrfInitn></Document>";
        for got in all_four(pain) {
            let err = got.expect_err("a pain.001 is not a statement");
            assert!(err.contains("no <Stmt>, <Ntfctn> or <Rpt> found"), "{err}");
        }
    }

    /// The other half of that contract: a statement that holds none of the
    /// record a reader is after returns nothing, and is not an error. An account
    /// with no movements is a statement.
    #[test]
    fn camt_a_statement_with_no_records_of_this_kind_returns_nothing() {
        let balances_only = camt053(
            "<Stmt><Id>S-1</Id><Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp>\
             <Amt Ccy=\"CHF\">10.00</Amt><CdtDbtInd>CRDT</CdtDbtInd></Bal></Stmt>",
        );
        assert_eq!(
            all_four(&balances_only).map(|got| got.expect("a statement is a statement")),
            [0, 1, 0, 0]
        );
    }

    /// The inner container is not matched against the family. A camt.053 states
    /// `<Stmt>`, but `read_iso20022` scopes a `<Rpt>` it finds in one anyway,
    /// and the four readers join on those columns: skipping the container here
    /// while the entry reader numbered it would attach the next statement's
    /// records to it. Reported as the kind it is spelled, and the caller can
    /// see the disagreement with the family for itself.
    #[test]
    fn camt_a_container_the_family_does_not_state_is_still_that_account() {
        let wrong = camt053(
            "<Rpt><Id>R-1</Id><Bal><Tp><CdOrPrtry><Cd>ITBD</Cd></CdOrPrtry></Tp>\
             <Amt Ccy=\"CHF\">1.00</Amt></Bal></Rpt>",
        );
        assert_eq!(
            balances(&wrong).expect("a container is a container"),
            [(1, 1, Some("ITBD".to_string()), Some(100_000))]
        );

        let report = "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.052.001.08\">\
                      <BkToCstmrAcctRpt><GrpHdr><MsgId>R-0</MsgId></GrpHdr>\
                      <Rpt><Id>R-1</Id><Bal><Tp><CdOrPrtry><Cd>ITBD</Cd></CdOrPrtry></Tp>\
                      <Amt Ccy=\"CHF\">1.00</Amt></Bal></Rpt></BkToCstmrAcctRpt></Document>";
        assert_eq!(
            balances(report).expect("a camt.052 states <Rpt>"),
            [(1, 1, Some("ITBD".to_string()), Some(100_000))]
        );
    }

    /// The three shapes the strict gate used to drop on the floor, each one a
    /// row in `read_iso20022`: a statement under a wrapper nothing identifies, a
    /// second message in one `<Document>` naming its own rows, and a
    /// namespace-free `<Document>` after an identified one.
    #[test]
    fn camt_a_statement_is_a_statement_whatever_named_the_file() {
        let national = "<Document><NtlStmtFile><GrpHdr><MsgId>M-1</MsgId></GrpHdr>\
                        <Stmt><Id>S-1</Id><Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp>\
                        <Amt Ccy=\"CHF\">1.00</Amt></Bal></Stmt></NtlStmtFile></Document>";
        assert_eq!(
            balances(national).expect("a <Stmt> is a statement with no namespace above it"),
            [(1, 1, Some("CLBD".to_string()), Some(100_000))]
        );

        let two_messages = "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>MSG-A</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id><Bal><Amt Ccy=\"CHF\">1.00</Amt></Bal></Stmt></BkToCstmrStmt>\
             <BkToCstmrStmt><GrpHdr><MsgId>MSG-B</MsgId></GrpHdr>\
             <Stmt><Id>S-2</Id><Bal><Amt Ccy=\"CHF\">2.00</Amt></Bal></Stmt></BkToCstmrStmt>\
             </Document>";
        assert_eq!(
            msg_ids(two_messages).expect("two headers name two messages"),
            ["MSG-A", "MSG-B"],
            "the second GrpHdr names the rows that follow it"
        );

        let after_identified = "<Batch>\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>A</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id><Bal><Amt Ccy=\"CHF\">1.00</Amt></Bal></Stmt></BkToCstmrStmt>\
             </Document><Document><NtlNtfctnFile><GrpHdr><MsgId>B</MsgId></GrpHdr>\
             <Ntfctn><Id>N-1</Id><Bal><Amt Ccy=\"CHF\">2.00</Amt></Bal></Ntfctn>\
             </NtlNtfctnFile></Document></Batch>";
        assert_eq!(
            msg_ids(after_identified).expect("both documents hold a container"),
            ["A", "B"],
            "the first Document's binding is not the second Document's identity"
        );
    }

    /// Two statements in one document are two statements. `statement_index` is
    /// source-global so the entry rows can be joined to them; the record index
    /// and the account context restart.
    #[test]
    fn camt_two_statements_are_numbered_across_the_file_and_reset_inside_it() {
        let two = camt053(&format!(
            "<Stmt><Id>S-1</Id><Acct><Id><IBAN>CH11</IBAN></Id><Ccy>CHF</Ccy></Acct>\
             {ONE_ENTRY}{ONE_ENTRY}</Stmt>\
             <Stmt><Id>S-2</Id><Acct><Id><IBAN>CH22</IBAN></Id></Acct>{ONE_ENTRY}</Stmt>"
        ));
        assert_eq!(
            transactions(&two).expect("both statements parse"),
            [(1, 1, 1, 1), (1, 2, 1, 1), (2, 1, 1, 1)]
        );

        let mut stream = TransactionStream::new(Cursor::new(two.as_bytes()), "test.xml");
        let mut seen = Vec::new();
        while let Some(row) = stream.next_row().expect("both statements parse") {
            seen.push((
                row.statement_id.unwrap_or_default(),
                row.account_iban.unwrap_or_default(),
                row.account_currency,
                row.msg_id.unwrap_or_default(),
            ));
        }
        assert_eq!(
            seen,
            [
                (
                    "S-1".to_string(),
                    "CH11".to_string(),
                    Some("CHF".to_string()),
                    "M-1".to_string()
                ),
                (
                    "S-1".to_string(),
                    "CH11".to_string(),
                    Some("CHF".to_string()),
                    "M-1".to_string()
                ),
                // the second statement states no currency, and does not keep
                // the first one's
                (
                    "S-2".to_string(),
                    "CH22".to_string(),
                    None,
                    "M-1".to_string()
                ),
            ]
        );
    }

    /// The context is captured at exact scoped paths. A statement names its own
    /// account and may name a related one beside it; a tail match on
    /// `Acct/Id/IBAN` reads whichever came last.
    #[test]
    fn camt_a_related_account_does_not_overwrite_the_statements_own() {
        let xml = camt053(&format!(
            "<Stmt><Id>S-1</Id><Acct><Id><IBAN>CH11</IBAN></Id></Acct>\
             <RltdAcct><Id><IBAN>CH99</IBAN></Id></RltdAcct>{ONE_ENTRY}</Stmt>"
        ));
        let mut stream = TransactionStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let row = stream
            .next_row()
            .expect("the statement parses")
            .expect("one transaction");
        assert_eq!(row.account_iban.as_deref(), Some("CH11"));
    }

    /// Only direct children of the statement are records. An `<Ntry>` inside a
    /// transaction summary is a total and not a movement, and a `<Bal>` nested
    /// under one is not this account's position.
    #[test]
    fn camt_only_direct_children_of_a_statement_are_records() {
        let xml = camt053(&format!(
            "<Stmt><Id>S-1</Id>\
             <TxsSummry><TtlNtries><NbOfNtries>9</NbOfNtries></TtlNtries>{ONE_ENTRY}</TxsSummry>\
             {ONE_ENTRY}</Stmt>"
        ));
        assert_eq!(
            transactions(&xml).expect("the statement parses"),
            [(1, 1, 1, 1)],
            "the summary's entry is not a movement of this account"
        );
    }

    /// `<Bal/>` and `<Bal></Bal>` are the same balance stated two ways, so they
    /// have to number the same. The self-closing spelling used to produce no row
    /// and consume no index, which reported the third balance of a statement as
    /// its second - the index would have depended on how the writer closed its
    /// tags. `read_camt_amount_details` already emits a row for `<TxAmt/>` on
    /// exactly this reasoning.
    #[test]
    fn camt_a_self_closing_balance_is_still_a_balance() {
        let xml = camt053(
            "<Stmt><Id>S-1</Id>\
             <Bal><Tp><CdOrPrtry><Cd>OPBD</Cd></CdOrPrtry></Tp><Amt Ccy=\"CHF\">10.00</Amt></Bal>\
             <Bal/>\
             <Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp><Amt Ccy=\"CHF\">9.00</Amt></Bal>\
             </Stmt>",
        );
        assert_eq!(
            balances(&xml).expect("the statement parses"),
            [
                (1, 1, Some("OPBD".to_string()), Some(1_000_000)),
                (1, 2, None, None),
                (1, 3, Some("CLBD".to_string()), Some(900_000)),
            ]
        );
    }

    /// A namespace-free national wrapper, named by the header above it. The
    /// wrapper is not a container this knows, so `<Document>` is what says the
    /// message begins and the header is what says which message it is.
    #[test]
    fn camt_a_header_names_a_namespace_free_statement() {
        let xml = format!(
            "<BizMsgEnvlp><AppHdr><MsgDefIdr>camt.053.001.08</MsgDefIdr></AppHdr>\
             <Document><NtlStmtFile><GrpHdr><MsgId>N-1</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id>{ONE_ENTRY}</Stmt></NtlStmtFile></Document></BizMsgEnvlp>"
        );
        assert_eq!(
            transactions(&xml).expect("the header named it"),
            [(1, 1, 1, 1)]
        );
    }

    /// The other record of a statement is consumed and dropped, and a truncation
    /// inside the skipped subtree is still a truncation. A skip that returned
    /// quietly would turn a cut-off download into a short result.
    #[test]
    fn camt_a_truncation_inside_a_skipped_record_is_still_an_error() {
        let cut = "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
                   <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr><Stmt><Id>S-1</Id>\
                   <Ntry><NtryRef>N-1</NtryRef><Amt Ccy=\"CHF\">1.00";
        let err = balances(cut).expect_err("the file stops inside the skipped entry");
        assert!(
            err.contains("<Ntry>"),
            "the refusal names the subtree: {err}"
        );
    }

    /// Malformed money at each of the four sites a supplementary row projects,
    /// one temporary statement each, so a failure names one constructor.
    #[test]
    fn camt_malformed_money_is_refused_at_every_site_that_projects_it() {
        let bad = "10.1234567";
        for (site, stmt, wanted) in [
            (
                "balance amount",
                format!("<Stmt><Id>S</Id><Bal><Amt Ccy=\"CHF\">{bad}</Amt></Bal></Stmt>"),
                [false, true, false, false],
            ),
            (
                "transaction amount",
                format!(
                    "<Stmt><Id>S</Id><Ntry><NtryDtls><TxDtls><Amt Ccy=\"CHF\">{bad}</Amt>\
                     </TxDtls></NtryDtls></Ntry></Stmt>"
                ),
                [true, false, false, false],
            ),
            (
                "batch total",
                format!(
                    "<Stmt><Id>S</Id><Ntry><NtryDtls><Btch><TtlAmt Ccy=\"CHF\">{bad}</TtlAmt>\
                     </Btch><TxDtls></TxDtls></NtryDtls></Ntry></Stmt>"
                ),
                [true, false, false, false],
            ),
            (
                "entry amount detail",
                format!(
                    "<Stmt><Id>S</Id><Ntry><AmtDtls><TxAmt><Amt Ccy=\"CHF\">{bad}</Amt>\
                     </TxAmt></AmtDtls></Ntry></Stmt>"
                ),
                [false, false, true, false],
            ),
            (
                "transaction amount detail",
                format!(
                    "<Stmt><Id>S</Id><Ntry><NtryDtls><TxDtls><AmtDtls>\
                     <InstdAmt><Amt Ccy=\"CHF\">{bad}</Amt></InstdAmt></AmtDtls>\
                     </TxDtls></NtryDtls></Ntry></Stmt>"
                ),
                [false, false, true, false],
            ),
        ] {
            let xml = camt053(&stmt);
            for (got, should_fail) in all_four(&xml).into_iter().zip(wanted) {
                match (got, should_fail) {
                    (Err(e), true) => assert!(
                        e.contains("7 fraction digits") && e.contains("test.xml"),
                        "{site}: {e}"
                    ),
                    (Err(e), false) => panic!("{site}: refused a value it does not project: {e}"),
                    (Ok(_), true) => panic!("{site}: a malformed amount became a NULL"),
                    (Ok(_), false) => {}
                }
            }
        }
    }

    /// An entry whose money is malformed and whose transactions, balances and
    /// amount blocks are all absent is refused by the entry reader and by none
    /// of these four: the amount is not on any row they build.
    #[test]
    fn camt_an_entry_amount_nothing_here_projects_is_not_read() {
        let xml = camt053(
            "<Stmt><Id>S</Id><Ntry><NtryRef>BAD</NtryRef>\
             <Amt Ccy=\"CHF\">10.1234567</Amt></Ntry></Stmt>",
        );
        assert_eq!(
            all_four(&xml).map(|got| got.expect("no row carries that amount")),
            [0, 0, 0, 0]
        );
    }
}
