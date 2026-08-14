//! camt.036 — Debit Authorisation Response. The customer answering the
//! camt.037 its bank sent: yes, you may take the money back off my account.
//!
//! Shape:
//!
//! ```text
//! DbtAuthstnRspn
//!   Assgnmt              — who answers whom
//!   Case                 — the case the request belongs to
//!   Conf/DbtAuthstn      — the answer, as the wire spelled it
//! ```
//!
//! `debit_authorised` is text, not a boolean, for the same reason
//! `group_cancellation` is in read_camt056: the wire has one spelling and a
//! typed column would have to invent a value for the absent case.
//!
//! The schema also allows the response to restate the amount and value date it
//! agrees to, but no published sample carries either — a column no fixture
//! populates is a column no test can be wrong about, so there is none.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::wire::{self, AssignCtx, CaseCtx};

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct DbtRspnRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    /// "true" or "false", as the wire spelled it.
    pub debit_authorised: Option<String>,
    pub source_file: Option<String>,
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct DbtRspnStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    debit_authorised: Option<String>,
    saw_response: bool,
    /// `path.len()` at the innermost open container of this family.
    in_response: Option<usize>,
}

impl<R: BufRead> DbtRspnStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        DbtRspnStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            debit_authorised: None,
            saw_response: false,
            in_response: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<DbtRspnRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match self.reader.read_event_into(&mut self.buf)? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    Act::Push(wire::local(qname.as_ref()).into_owned())
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_response.is_some()
                        && (name == "DbtAuthstnRspn" || name.starts_with("camt.036."))
                    {
                        Act::Close
                    } else {
                        Act::Pop
                    }
                }
                ev => match wire::event_text(&ev)? {
                    Some(t) => Act::Text(t),
                    None => Act::None,
                },
            };

            match action {
                Act::Eof => {
                    return if self.saw_response {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <DbtAuthstnRspn> found — is this a camt.036 debit \
                             authorisation response?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Push(name) => {
                    if name == "DbtAuthstnRspn" || name.starts_with("camt.036.") {
                        self.saw_response = true;
                        self.in_response = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.debit_authorised = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(DbtRspnRow {
                        assignment_id: self.assign.id.clone(),
                        assignment_created: self.assign.created.clone(),
                        assigner: self.assign.assigner.clone(),
                        assignee: self.assign.assignee.clone(),
                        case_id: self.case.id.clone(),
                        case_creator: self.case.creator.clone(),
                        debit_authorised: self.debit_authorised.clone(),
                        source_file: Some(self.source.clone()),
                    }));
                }
                Act::Pop => self.pop(),
                Act::Text(t) => self.capture(&t),
                Act::None => {}
            }
        }
    }

    fn pop(&mut self) {
        self.path.pop();
        if self.in_response == Some(self.path.len()) {
            self.in_response = None;
        }
    }

    fn capture(&mut self, text: &str) {
        if wire::capture_assignment(&mut self.assign, &self.path, text)
            || wire::capture_case(&mut self.case, &self.path, text)
        {
            return;
        }
        if wire::ends_with(&self.path, &["Conf", "DbtAuthstn"]) {
            self.debit_authorised = Some(text.to_string());
        }
    }
}

enum Act {
    Eof,
    Push(String),
    Pop,
    Close,
    Text(String),
    None,
}
