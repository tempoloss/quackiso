//! camt.057 - Notification to Receive. A bank telling an account holder that
//! money is on its way: what is expected, on which account, and when it should
//! land. It is a funding message rather than a settlement one - nothing here has
//! been booked yet, which is what separates it from camt.054.
//!
//! The mid level is `Ntfctn`, one notification per account. It names the account
//! once, then holds many `Itm` children that each announce one expected credit,
//! so the reader carries the account context downward, the same way the pain.001
//! reader carries its payment group.
//!
//! Grain: one row per Itm.

use std::error::Error;
use std::io::BufRead;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;

use crate::wire::{self, money, DateOrText, Money, PartyName, Reason};

// ── serde model: the item subtree only ───────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Itm {
    #[serde(rename = "Id")]
    pub id: Option<String>,
    #[serde(rename = "Amt")]
    pub amt: Option<Money>,
    #[serde(rename = "XpctdValDt")]
    pub xpctd_val_dt: Option<DateOrText>,
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<PartyName>,
    #[serde(rename = "Purp")]
    pub purp: Option<Reason>,
}

// ── flattened row ────────────────────────────────────────────────────────────

/// Notification-level context carried into every item beneath it.
#[derive(Debug, Default, Clone)]
pub struct NtfctnCtx {
    pub msg_id: Option<String>,
    pub notification_id: Option<String>,
    pub account: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct NtfctnRow {
    pub msg_id: Option<String>,
    pub notification_id: Option<String>,
    pub account: Option<String>,
    pub item_id: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    /// A date, or a date and time when the sender named an hour.
    pub expected_value_date: Option<String>,
    pub debtor_name: Option<String>,
    pub purpose: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_item(item: &Itm, ctx: &NtfctnCtx, source: &str) -> Result<NtfctnRow, String> {
    let (amount, currency) = money(&[item.amt.as_ref()]).map_err(|e| format!("{source}: {e}"))?;

    Ok(NtfctnRow {
        msg_id: ctx.msg_id.clone(),
        notification_id: ctx.notification_id.clone(),
        account: ctx.account.clone(),
        item_id: item.id.clone(),
        amount,
        currency,
        expected_value_date: item.xpctd_val_dt.as_ref().and_then(DateOrText::value),
        debtor_name: item.dbtr.as_ref().and_then(PartyName::name),
        purpose: item.purp.as_ref().and_then(Reason::code),
        source_file: Some(source.to_string()),
    })
}

// ── streaming reader ─────────────────────────────────────────────────────────

pub struct NtfctnStream<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    path: Vec<String>,
    source: String,
    ctx: NtfctnCtx,
    /// Seen anywhere in the file; only the EOF check reads it.
    saw_notification: bool,
    /// `path.len()` at the innermost open container of this family.
    /// `Itm` is a short, generic name that other ISO 20022 messages use for
    /// their own lists, so without this guard a foreign subtree would produce
    /// rows.
    in_notification: Option<usize>,
}

impl<R: BufRead> NtfctnStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        NtfctnStream {
            reader: Reader::from_reader(reader),
            buf: Vec::with_capacity(8 * 1024),
            path: Vec::with_capacity(16),
            source: source.to_string(),
            ctx: NtfctnCtx::default(),
            saw_notification: false,
            in_notification: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<NtfctnRow>, Box<dyn Error>> {
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
                    let name = wire::local(qname.as_ref());
                    if name == "Itm" && self.in_notification.is_some() {
                        Act::Item
                    } else {
                        Act::Push(name.into_owned())
                    }
                }
                Event::End(_) => Act::Pop,
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
                            "{}: no <NtfctnToRcv> found - is this a camt.057 notification \
                             to receive?",
                            self.source
                        )
                        .into())
                    }
                }
                Act::Item => return Ok(Some(self.read_item()?)),
                Act::Push(name) => {
                    if name == "NtfctnToRcv" || name.starts_with("camt.057.") {
                        self.saw_notification = true;
                        self.in_notification = Some(self.path.len());
                        self.ctx = NtfctnCtx::default();
                    }
                    // a new notification replaces the previous account
                    if name == "Ntfctn" {
                        let msg_id = self.ctx.msg_id.clone();
                        self.ctx = NtfctnCtx {
                            msg_id,
                            ..Default::default()
                        };
                    }
                    self.path.push(name);
                }
                Act::Pop => {
                    self.pop();
                }
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

    /// Capture notification-level leaves by path tail. Item-internal elements
    /// live inside the `<Itm>` subtree, which never enters `path`, so these tails
    /// cannot collide with an item's own id.
    fn capture(&mut self, text: &str) {
        let p = &self.path;
        let tail = |suffix: &[&str]| wire::ends_with(p, suffix);

        if tail(&["GrpHdr", "MsgId"]) {
            self.ctx.msg_id = Some(text.to_string());
        } else if tail(&["Ntfctn", "Id"]) {
            self.ctx.notification_id = Some(text.to_string());
        } else if tail(&["Acct", "Id", "IBAN"]) {
            self.ctx.account = Some(text.to_string());
        } else if tail(&["Acct", "Id", "Othr", "Id"]) {
            // a custody or in-house account number has no IBAN to lose to
            self.ctx.account.get_or_insert_with(|| text.to_string());
        }
    }

    /// Record the `<Itm>` subtree and deserialize it.
    fn read_item(&mut self) -> Result<NtfctnRow, Box<dyn Error>> {
        let xml = wire::record_subtree(&mut self.reader, &mut self.buf, "Itm", &self.source)?;
        let item: Itm = quick_xml::de::from_str(&xml)?;
        Ok(row_from_item(&item, &self.ctx, &self.source)?)
    }
}

enum Act {
    Eof,
    Item,
    Push(String),
    Pop,
    Text(String),
    None,
}
