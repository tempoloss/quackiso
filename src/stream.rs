//! Streaming camt.05x entry reader. Walks the XML as events over a buffered
//! reader, carries the small statement/message context inline, and deserializes
//! only one `<Ntry>` subtree at a time. Memory is O(one entry), not O(file), so
//! a multi-GB statement costs the same as a small one.
//!
//! This is the looser of the two camt walks and ADR 0004 says why: an `<Ntry>`
//! becomes a row wherever it appears, including one nested where no schema puts
//! it, because a reader that silently dropped an entry would under-report an
//! account. `camt::StatementRecordStream` is the strict walk the supplementary
//! readers use, and the two meet at the three scope columns: an entry those
//! readers would emit is exactly an entry whose `statement_kind`,
//! `statement_index` and `entry_index` are not NULL here. An out-of-statement
//! entry is still a row, with those three NULL and no scoped index consumed -
//! a join key pointing at nothing is worse than an absent one.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::{row_from_entry, EntryCtx, Ntry, Row};
use crate::wire;

pub struct EntryStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    /// element local-names from root to cursor (entries excluded — those are
    /// consumed as a subtree, never pushed here)
    path: Vec<String>,
    source: String,
    msg_id: Option<String>,
    account_iban: Option<String>,
    statement_id: Option<String>,
    /// Whether this file held a statement, notification or report at all. An
    /// account with no movements is a valid statement with no entries, so the
    /// check is on the container and not on the entries: a message of the wrong
    /// type must fail loudly, an empty statement must not.
    saw_container: bool,
    /// The active container's name and `path.len()`, and how many statements
    /// this file has opened. Source-global and 1-based, so two statements in
    /// one document are 1 and 2.
    container_kind: Option<String>,
    container_depth: Option<usize>,
    statement_index: i64,
    /// Entries numbered inside the active container. Only direct children
    /// consume an index.
    entry_index: i64,
}

impl<R: BufRead> EntryStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        EntryStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            msg_id: None,
            account_iban: None,
            statement_id: None,
            saw_container: false,
            container_kind: None,
            container_depth: None,
            statement_index: 0,
            entry_index: 0,
        }
    }

    /// Pull the next entry row, or None at end of document.
    pub fn next_row(&mut self) -> Result<Option<Row>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            // Resolve the event into owned data so the borrow on `buf` ends
            // before we call back into `self` (read_entry needs &mut self).
            let action = match wire::next_event(
                &mut self.reader,
                &mut self.buf,
                &self.path,
                &self.source,
            )? {
                Event::Eof => Action::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if name == "Ntry" {
                        Action::Entry
                    } else {
                        Action::Push(name.into_owned())
                    }
                }
                Event::End(_) => Action::Pop,
                ev => match wire::event_text(&ev)? {
                    Some(t) => Action::Text(t),
                    None => Action::None,
                },
            };

            match action {
                Action::Eof => {
                    return if self.saw_container {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <Stmt>, <Ntfctn> or <Rpt> found — is this a \
                             camt.053/.054/.052 message?",
                            self.source
                        )
                        .into())
                    }
                }
                Action::Entry => return Ok(Some(self.read_entry()?)),
                Action::Push(name) => {
                    // camt.053 uses <Stmt>, camt.054 <Ntfctn>, camt.052 <Rpt>.
                    // All three carry the same <Ntry> children, so one reader
                    // covers the family; only the container name differs.
                    if is_container(&name) && self.container_depth.is_none() {
                        self.saw_container = true;
                        self.statement_id = None;
                        self.account_iban = None;
                        self.container_kind = Some(name.clone());
                        self.container_depth = Some(self.path.len());
                        self.statement_index += 1;
                        self.entry_index = 0;
                    }
                    self.path.push(name);
                }
                Action::Pop => {
                    self.path.pop();
                    if self.container_depth == Some(self.path.len()) {
                        self.container_depth = None;
                        self.container_kind = None;
                        // The id and the account go with it. An entry after
                        // `</Stmt>` reports NULL scope columns, and naming the
                        // closed statement's account beside them would book its
                        // money to an account it was never posted to.
                        self.statement_id = None;
                        self.account_iban = None;
                    }
                }
                Action::Text(t) => self.capture(&t),
                Action::None => {}
            }
        }
    }

    /// Record the current `<Ntry>` subtree and deserialize it.
    fn read_entry(&mut self) -> Result<Row, Box<dyn Error>> {
        // Scoped only when the entry is a direct child of the active container.
        // Everything else is still a row, with the three scope columns NULL:
        // numbering an entry nobody can join to would make the columns look
        // usable and be wrong once.
        let scoped = self
            .container_depth
            .is_some_and(|depth| self.path.len() == depth + 1);
        if scoped {
            self.entry_index += 1;
        }
        let ctx = EntryCtx {
            msg_id: self.msg_id.clone(),
            account_iban: self.account_iban.clone(),
            statement_id: self.statement_id.clone(),
            statement_kind: scoped.then(|| self.container_kind.clone()).flatten(),
            statement_index: scoped.then_some(self.statement_index),
            entry_index: scoped.then_some(self.entry_index),
        };
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Ntry", &self.source)?;
        let entry: Ntry = quick_xml::de::from_str(&xml)?;
        Ok(row_from_entry(&entry, &ctx, &self.source)?)
    }

    /// Capture the three context leaves by matching the path tail. Their tails
    /// are unambiguous because entry-internal IBAN/Nm live inside the `<Ntry>`
    /// subtree, which never enters `self.path`.
    fn capture(&mut self, text: &str) {
        let which = {
            let p = &self.path;
            if wire::ends_with(p, &["GrpHdr", "MsgId"]) {
                1
            } else if p.len() >= 2 && p[p.len() - 1] == "Id" && is_container(&p[p.len() - 2]) {
                2
            } else if wire::ends_with(p, &["Acct", "Id", "IBAN"]) {
                3
            } else if wire::ends_with(p, &["Acct", "Id", "Othr", "Id"]) {
                4
            } else {
                0
            }
        };
        match which {
            1 => self.msg_id = Some(text.to_string()),
            2 => self.statement_id = Some(text.to_string()),
            3 => self.account_iban = Some(text.to_string()),
            // non-IBAN account number; only if no IBAN was seen for this stmt
            4 if self.account_iban.is_none() => self.account_iban = Some(text.to_string()),
            _ => {}
        }
    }
}

/// The per-account container that holds `<Ntry>` children. camt.053 calls it
/// `Stmt`, camt.054 `Ntfctn`, camt.052 `Rpt` — otherwise they are the same
/// shape, so one reader serves all three.
fn is_container(name: &str) -> bool {
    matches!(name, "Stmt" | "Ntfctn" | "Rpt")
}

enum Action {
    Eof,
    Entry,
    Push(String),
    Pop,
    Text(String),
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// One entry, as its reference and the three scope columns.
    type Scope = (String, Option<String>, Option<i64>, Option<i64>);

    /// Every entry of `xml`, in the order the reader emitted them.
    fn scopes(xml: &str) -> Vec<Scope> {
        let mut stream = EntryStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the statement parses") {
            out.push((
                row.entry_ref.unwrap_or_default(),
                row.statement_kind,
                row.statement_index,
                row.entry_index,
            ));
        }
        out
    }

    fn entry(reference: &str) -> String {
        format!("<Ntry><NtryRef>{reference}</NtryRef><Amt Ccy=\"CHF\">1.00</Amt></Ntry>")
    }

    /// ADR 0004 keeps every `<Ntry>` as a row wherever it appears, and the scope
    /// columns are how a caller tells the two kinds apart. An entry inside a
    /// transaction summary and one outside the statement altogether are both
    /// rows, both with all three NULL, and neither takes an index off the
    /// entries that can be joined to.
    #[test]
    fn stream_an_unscoped_entry_is_a_row_with_no_scope_and_takes_no_index() {
        let xml = format!(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id>{}<TxsSummry>{}</TxsSummry>{}</Stmt>{}\
             </BkToCstmrStmt></Document>",
            entry("direct-1"),
            entry("in-summary"),
            entry("direct-2"),
            entry("outside")
        );
        assert_eq!(
            scopes(&xml),
            [
                (
                    "direct-1".to_string(),
                    Some("Stmt".to_string()),
                    Some(1),
                    Some(1)
                ),
                ("in-summary".to_string(), None, None, None),
                (
                    "direct-2".to_string(),
                    Some("Stmt".to_string()),
                    Some(1),
                    Some(2)
                ),
                ("outside".to_string(), None, None, None),
            ]
        );
    }

    /// The index is source-global for statements and restarts for entries, and
    /// nothing carries over: a stale index on the first entry of the second
    /// statement would point every supplementary row at the wrong account.
    #[test]
    fn stream_a_second_statement_numbers_its_own_entries() {
        let xml = format!(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id>{}{}</Stmt><Stmt><Id>S-2</Id>{}</Stmt>\
             </BkToCstmrStmt></Document>",
            entry("a"),
            entry("b"),
            entry("c")
        );
        assert_eq!(
            scopes(&xml),
            [
                ("a".to_string(), Some("Stmt".to_string()), Some(1), Some(1)),
                ("b".to_string(), Some("Stmt".to_string()), Some(1), Some(2)),
                ("c".to_string(), Some("Stmt".to_string()), Some(2), Some(1)),
            ]
        );
    }

    /// A row that says it belongs to no statement must not name one. The
    /// account is the column a caller groups money by, and an entry after
    /// `</Stmt>` was carrying the closed statement's IBAN beside three NULL
    /// scope columns - `SUM(amount) GROUP BY account_iban` booked it to an
    /// account it was never posted to.
    #[test]
    fn stream_an_entry_after_the_statement_names_no_account() {
        let xml = format!(
            "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>M-1</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id><Acct><Id><IBAN>DE02120300000000202051</IBAN></Id></Acct>\
             {}<TxsSummry>{}</TxsSummry></Stmt>{}\
             </BkToCstmrStmt></Document>",
            entry("direct"),
            entry("in-summary"),
            entry("outside")
        );
        let mut stream = EntryStream::new(Cursor::new(xml.as_bytes()), "test.xml");
        let mut got = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            got.push((row.entry_ref, row.statement_id, row.account_iban));
        }
        let iban = || Some("DE02120300000000202051".to_string());
        assert_eq!(
            got,
            [
                (Some("direct".to_string()), Some("S-1".to_string()), iban()),
                // still inside the statement, so the account is still its own
                (
                    Some("in-summary".to_string()),
                    Some("S-1".to_string()),
                    iban()
                ),
                (Some("outside".to_string()), None, None),
            ]
        );
    }

    /// The three container spellings, each reported as the wire spelled it.
    #[test]
    fn stream_the_statement_kind_is_the_container_name() {
        for container in ["Stmt", "Ntfctn", "Rpt"] {
            let xml = format!(
                "<Document><BkToCstmrStmt><{container}><Id>X</Id>{}</{container}>\
                 </BkToCstmrStmt></Document>",
                entry("e")
            );
            assert_eq!(
                scopes(&xml),
                [(
                    "e".to_string(),
                    Some(container.to_string()),
                    Some(1),
                    Some(1)
                )]
            );
        }
    }
}
