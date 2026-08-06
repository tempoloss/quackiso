//! Streaming camt.053 reader. Walks the XML as events over a buffered reader,
//! carries the small statement/message context inline, and deserializes only
//! one `<Ntry>` subtree at a time. Memory is O(one entry), not O(file), so a
//! multi-GB statement costs the same as a small one.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::{row_from_entry, Ntry, Row};
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
        }
    }

    /// Pull the next entry row, or None at end of document.
    pub fn next_row(&mut self) -> Result<Option<Row>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            // Resolve the event into owned data so the borrow on `buf` ends
            // before we call back into `self` (read_entry needs &mut self).
            let action = match self.reader.read_event_into(&mut self.buf)? {
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
                    if is_container(&name) {
                        self.saw_container = true;
                        self.statement_id = None;
                        self.account_iban = None;
                    }
                    self.path.push(name);
                }
                Action::Pop => {
                    self.path.pop();
                }
                Action::Text(t) => self.capture(&t),
                Action::None => {}
            }
        }
    }

    /// Record the current `<Ntry>` subtree and deserialize it.
    fn read_entry(&mut self) -> Result<Row, Box<dyn Error>> {
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Ntry")?;
        let entry: Ntry = quick_xml::de::from_str(&xml)?;
        Ok(row_from_entry(
            &entry,
            &self.msg_id,
            &self.account_iban,
            &self.statement_id,
            &self.source,
        )?)
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
