//! Streaming camt.053 reader. Walks the XML as events over a buffered reader,
//! carries the small statement/message context inline, and deserializes only
//! one `<Ntry>` subtree at a time. Memory is O(one entry), not O(file), so a
//! multi-GB statement costs the same as a small one.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use quick_xml::Writer;

use crate::model::{row_from_entry, Ntry, Row};

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
                    let name = local(e.name().as_ref());
                    if name == "Ntry" {
                        Action::Entry
                    } else {
                        Action::Push(name)
                    }
                }
                Event::End(_) => Action::Pop,
                Event::Text(e) => {
                    let t = e.unescape()?;
                    let t = t.trim();
                    if t.is_empty() {
                        Action::None
                    } else {
                        Action::Text(t.to_string())
                    }
                }
                _ => Action::None,
            };

            match action {
                Action::Eof => return Ok(None),
                Action::Entry => return Ok(Some(self.read_entry()?)),
                Action::Push(name) => {
                    if name == "Stmt" {
                        // new statement: its context replaces the previous one
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

    /// Record the current `<Ntry>` subtree to a buffer and deserialize it. The
    /// start tag was already consumed by the caller; Ntry carries no attributes
    /// in camt.053, so we re-emit a bare `<Ntry>`.
    fn read_entry(&mut self) -> Result<Row, Box<dyn Error>> {
        let mut w = Writer::new(Vec::new());
        w.write_event(Event::Start(BytesStart::new("Ntry")))?;
        let mut depth = 1;
        loop {
            self.buf.clear();
            let ev = self.reader.read_event_into(&mut self.buf)?;
            match &ev {
                Event::Start(e) if local(e.name().as_ref()) == "Ntry" => depth += 1,
                Event::End(e) if local(e.name().as_ref()) == "Ntry" => depth -= 1,
                Event::Eof => return Err("unexpected EOF inside <Ntry>".into()),
                _ => {}
            }
            w.write_event(ev)?;
            if depth == 0 {
                break;
            }
        }
        let xml = String::from_utf8(w.into_inner())?;
        let entry: Ntry = quick_xml::de::from_str(&xml)?;
        Ok(row_from_entry(
            &entry,
            &self.msg_id,
            &self.account_iban,
            &self.statement_id,
            &self.source,
        ))
    }

    /// Capture the three context leaves by matching the path tail. Their tails
    /// are unambiguous because entry-internal IBAN/Nm live inside the `<Ntry>`
    /// subtree, which never enters `self.path`.
    fn capture(&mut self, text: &str) {
        let which = {
            let p = &self.path;
            if ends_with(p, &["GrpHdr", "MsgId"]) {
                1
            } else if ends_with(p, &["Stmt", "Id"]) {
                2
            } else if ends_with(p, &["Acct", "Id", "IBAN"]) {
                3
            } else if ends_with(p, &["Acct", "Id", "Othr", "Id"]) {
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
            4 => {
                if self.account_iban.is_none() {
                    self.account_iban = Some(text.to_string());
                }
            }
            _ => {}
        }
    }
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.into_owned(),
    }
}

fn ends_with(path: &[String], suffix: &[&str]) -> bool {
    path.len() >= suffix.len()
        && path[path.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(a, b)| a == b)
}

enum Action {
    Eof,
    Entry,
    Push(String),
    Pop,
    Text(String),
    None,
}
