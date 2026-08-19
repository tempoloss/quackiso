//! camt.030 — Notification of Case Assignment. Telling the party that opened a
//! case what has happened to it: it was passed on, closed, or taken over.
//!
//! Shape:
//!
//! ```text
//! NtfctnOfCaseAssgnmt
//!   Hdr              — who is notified, by whom, and when
//!   Case             — the case, and who opened it
//!   Assgnmt          — who the case is now assigned to, by whom
//!   Ntfctn/Justfn    — why, as a bare code
//! ```
//!
//! **Two party pairs, and they are not the same pair.** `Hdr/Fr` and `Hdr/To`
//! are the notification; `Assgnr` and `Assgne` are the assignment. In the real
//! sample the notification goes to EEEEUS33 while the case is assigned to
//! FFFFUS33, so collapsing them into one pair would report the wrong bank.
//!
//! `Justfn` here is a bare code (`CANC`, `FTHI`, `MINE`); camt.031 wraps its
//! justification in `RjctnRsn` instead.
//!
//! Grain: one row per message.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::wire::{self, AssignCtx, CaseCtx};

// ── flattened row ────────────────────────────────────────────────────────────

/// The notification header: the second party pair, kept apart from the
/// assignment's.
#[derive(Debug, Default, Clone)]
pub struct HdrCtx {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub created: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CaseNtfctnRow {
    pub assignment_id: Option<String>,
    pub assignment_created: Option<String>,
    pub assigner: Option<String>,
    pub assignee: Option<String>,
    pub case_id: Option<String>,
    pub case_creator: Option<String>,
    pub notification_id: Option<String>,
    pub notification_from: Option<String>,
    pub notification_to: Option<String>,
    pub notification_created: Option<String>,
    /// Why the case moved: a bare code.
    pub justification: Option<String>,
    pub source_file: Option<String>,
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct CaseNtfctnStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    assign: AssignCtx,
    case: CaseCtx,
    hdr: HdrCtx,
    justification: Option<String>,
    saw_notification: bool,
    /// `path.len()` at the innermost open container of this family.
    in_notification: Option<usize>,
}

impl<R: BufRead> CaseNtfctnStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        CaseNtfctnStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            assign: AssignCtx::default(),
            case: CaseCtx::default(),
            hdr: HdrCtx::default(),
            justification: None,
            saw_notification: false,
            in_notification: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<CaseNtfctnRow>, Box<dyn Error>> {
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
                    if self.in_notification.is_some()
                        && (name == "NtfctnOfCaseAssgnmt" || name.starts_with("camt.030."))
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
                    return if self.saw_notification {
                        Ok(None)
                    } else {
                        Err(format!(
                            "{}: no <NtfctnOfCaseAssgnmt> found — is this a camt.030 \
                             notification of case assignment?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Push(name) => {
                    if name == "NtfctnOfCaseAssgnmt" || name.starts_with("camt.030.") {
                        self.saw_notification = true;
                        self.in_notification = Some(self.path.len());
                        self.assign = AssignCtx::default();
                        self.case = CaseCtx::default();
                        self.hdr = HdrCtx::default();
                        self.justification = None;
                    }
                    self.path.push(name);
                }
                Act::Close => {
                    self.pop();
                    return Ok(Some(CaseNtfctnRow {
                        assignment_id: self.assign.id.clone(),
                        assignment_created: self.assign.created.clone(),
                        assigner: self.assign.assigner.clone(),
                        assignee: self.assign.assignee.clone(),
                        case_id: self.case.id.clone(),
                        case_creator: self.case.creator.clone(),
                        notification_id: self.hdr.id.clone(),
                        notification_from: self.hdr.from.clone(),
                        notification_to: self.hdr.to.clone(),
                        notification_created: self.hdr.created.clone(),
                        justification: self.justification.clone(),
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
        if self.in_notification == Some(self.path.len()) {
            self.in_notification = None;
        }
    }

    fn capture(&mut self, text: &str) {
        if wire::capture_assignment(&mut self.assign, &self.path, text)
            || wire::capture_case(&mut self.case, &self.path, text)
        {
            return;
        }
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["Hdr", "Id"]) {
            self.hdr.id = Some(text.to_string());
        } else if tail(&["Hdr", "CreDtTm"]) {
            self.hdr.created = Some(text.to_string());
        } else if tail(&["Fr", "Agt", "FinInstnId", "BICFI"])
            || tail(&["Fr", "Agt", "FinInstnId", "BIC"])
            || tail(&["Fr", "Pty", "Nm"])
        {
            self.hdr.from = Some(text.to_string());
        } else if tail(&["Fr", "Pty", "Id", "OrgId", "AnyBIC"])
            || tail(&["Fr", "Pty", "Id", "OrgId", "BICOrBEI"])
        {
            self.hdr.from.get_or_insert_with(|| text.to_string());
        } else if tail(&["To", "Agt", "FinInstnId", "BICFI"])
            || tail(&["To", "Agt", "FinInstnId", "BIC"])
            || tail(&["To", "Pty", "Nm"])
        {
            self.hdr.to = Some(text.to_string());
        } else if tail(&["To", "Pty", "Id", "OrgId", "AnyBIC"])
            || tail(&["To", "Pty", "Id", "OrgId", "BICOrBEI"])
        {
            self.hdr.to.get_or_insert_with(|| text.to_string());
        } else if tail(&["Ntfctn", "Justfn"]) {
            self.justification = Some(text.to_string());
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
