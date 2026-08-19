//! camt.031 — Reject Investigation. The assignee refusing a case: it will not
//! be worked, and this says why.
//!
//! Shape:
//!
//! ```text
//! RjctInvstgtn
//!   Assgnmt              — who refuses whom
//!   Case                 — the case being refused
//!   Justfn/RjctnRsn      — the reason code (NFND: the payment was not found)
//! ```
//!
//! There is no underlying payment block at all: the whole message is the
//! assignment, the case and the reason. The justification is wrapped in
//! `RjctnRsn` here, where camt.030 states a bare code.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::wire::{self, AssignCtx, CaseCtx};

// ── flattened row ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct RjctRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    pub rejection_reason: Option<String>,
    pub source_file: Option<String>,
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct RjctStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    rejection_reason: Option<String>,
    saw_rejection: bool,
    /// `path.len()` at the innermost open container of this family.
    in_rejection: Option<usize>,
}

impl<R: BufRead> RjctStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        RjctStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            rejection_reason: None,
            saw_rejection: false,
            in_rejection: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<RjctRow>, Box<dyn Error>> {
        loop {
            self.buf.clear();
            let action = match wire::next_event(
                &mut self.reader,
                &mut self.buf,
                &self.path,
                &self.source,
            )? {
                Event::Eof => Act::Eof,
                Event::Start(e) => {
                    let qname = e.name();
                    Act::Push(wire::local(qname.as_ref()).into_owned())
                }
                Event::End(e) => {
                    let qname = e.name();
                    let name = wire::local(qname.as_ref());
                    if self.in_rejection.is_some()
                        && (name == "RjctInvstgtn" || name.starts_with("camt.031."))
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
                    return if self.saw_rejection {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <RjctInvstgtn> found — is this a camt.031 rejection of \
                             an investigation?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Push(name) => {
                    if name == "RjctInvstgtn" || name.starts_with("camt.031.") {
                        self.saw_rejection = true;
                        self.in_rejection = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.rejection_reason = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(RjctRow {
                        assignment_id: self.assign.id.clone(),
                        assignment_created: self.assign.created.clone(),
                        assigner: self.assign.assigner.clone(),
                        assignee: self.assign.assignee.clone(),
                        case_id: self.case.id.clone(),
                        case_creator: self.case.creator.clone(),
                        rejection_reason: self.rejection_reason.clone(),
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
        if self.in_rejection == Some(self.path.len()) {
            self.in_rejection = None;
        }
    }

    fn capture(&mut self, text: &str) {
        if wire::capture_assignment(&mut self.assign, &self.path, text)
            || wire::capture_case(&mut self.case, &self.path, text)
        {
            return;
        }
        if wire::ends_with(&self.path, &["Justfn", "RjctnRsn"]) {
            self.rejection_reason = Some(text.to_string());
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
