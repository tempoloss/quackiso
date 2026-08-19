//! MT940 - Customer Statement Message. Account statement data with balances and
//! booked entries in SWIFT MT tag form.
//!
//! Grain: one row per `:61:` statement line.

use std::error::Error;
use std::io::BufRead;
use std::ops::Range;

use crate::mt::{self, Field, Fields};

/// Turns a field into the `path:line` prefix its errors carry.
type At<'a> = dyn Fn(&Field<'_>) -> String + 'a;

#[derive(Debug, Default, Clone)]
pub struct Mt940Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub tx_ref: Option<String>,
    pub related_ref: Option<String>,
    pub account: Option<String>,
    pub account_bic: Option<String>,
    pub statement_number: Option<i64>,
    pub sequence_number: Option<i64>,
    pub opening_balance_kind: Option<String>,
    pub opening_balance_dc: Option<String>,
    pub opening_balance_date: Option<i32>,
    pub opening_balance_currency: Option<String>,
    pub opening_balance: Option<i128>,
    pub closing_balance_kind: Option<String>,
    pub closing_balance_dc: Option<String>,
    pub closing_balance_date: Option<i32>,
    pub closing_balance_currency: Option<String>,
    pub closing_balance: Option<i128>,
    pub available_balance_dc: Option<String>,
    pub available_balance_date: Option<i32>,
    pub available_balance_currency: Option<String>,
    pub available_balance: Option<i128>,
    pub forward_available_dc: Option<String>,
    pub forward_available_date: Option<i32>,
    pub forward_available_currency: Option<String>,
    pub forward_available: Option<i128>,
    pub entry_index: Option<i64>,
    pub value_date: Option<i32>,
    pub entry_date: Option<i32>,
    pub credit_debit: Option<String>,
    pub funds_code: Option<String>,
    pub amount: Option<i128>,
    pub transaction_type: Option<String>,
    pub transaction_code: Option<String>,
    pub customer_ref: Option<String>,
    pub bank_ref: Option<String>,
    pub supplementary_details: Option<String>,
    pub narrative: Option<String>,
    pub statement_narrative: Option<String>,
    pub source_file: Option<String>,
}

/// The statement being emitted: its text, how far the entry walk has got, and the
/// row every entry starts from. Holding the text and a cursor is what bounds a
/// statement of half a million entries by its own bytes.
struct Statement {
    msg: String,
    body: Range<usize>,
    body_line: usize,
    message_line: usize,
    base: Mt940Row,
    cursor: mt::EntryCursor,
    emitted: i64,
}

pub struct Mt940Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
    open: Option<Statement>,
}

impl<R: BufRead> Mt940Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt940Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt940Row>, Box<dyn Error>> {
        loop {
            if let Some(open) = &mut self.open {
                match open.cursor.next_site(&open.msg[open.body.clone()], "61") {
                    Some(site) => {
                        open.emitted += 1;
                        let index = open.emitted;
                        return Ok(Some(row_from_entry(open, index, &site, &self.source)?));
                    }
                    None => {
                        self.open = None;
                        continue;
                    }
                }
            }

            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT940 message found - is this a SWIFT MT940 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_span(&msg, 4).unwrap_or((0..msg.len(), 1));
            let (fields, entries) = Fields::without_entries(&msg[body.clone()], "61");
            if !mt::claims(&msg, &fields, "940") {
                continue;
            }

            self.saw_my_type = true;
            let at = |field: &Field<'_>| {
                format!(
                    "{}:{}",
                    self.source,
                    mt::at(message_line, body_line, field.line)
                )
            };
            let base = base_row(&msg, &fields, &self.source, &at)?;
            // A claimed statement with no entries in it still reports the
            // balances the bank did send, which is one row.
            if entries == 0 {
                return Ok(Some(base));
            }
            self.open = Some(Statement {
                msg,
                body,
                body_line,
                message_line,
                base,
                cursor: mt::EntryCursor::default(),
                emitted: 0,
            });
        }
    }
}

fn base_row(msg: &str, fields: &Fields<'_>, source: &str, at: &At<'_>) -> Result<Mt940Row, String> {
    let mut row = Mt940Row {
        direction: mt::direction(msg).map(str::to_string),
        message_type: mt::message_type(msg).map(str::to_string),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: mt::user_header_field(msg, "121").map(str::to_string),
        validation_flag: mt::user_header_field(msg, "119").map(str::to_string),
        mur: mt::user_header_field(msg, "108").map(str::to_string),
        tx_ref: fields.value("20").and_then(text),
        related_ref: fields.value("21").and_then(text),
        statement_narrative: statement_narrative(fields),
        source_file: Some(source.to_string()),
        ..Default::default()
    };

    if let Some(field) = fields.find("25") {
        let (account, account_bic) = account(field.tag, &field.value);
        row.account = account;
        row.account_bic = account_bic;
    }

    if let Some(value) = fields.value("28") {
        let (statement_number, sequence_number) = mt::statement_number(value);
        row.statement_number = statement_number;
        row.sequence_number = sequence_number;
    }

    if let Some(field) = fields.find("60") {
        let balance = parse_balance(field, at)?;
        row.opening_balance_kind = kind(field.tag);
        row.opening_balance_dc = Some(balance.dc);
        row.opening_balance_date = balance.date;
        row.opening_balance_currency = Some(balance.currency);
        row.opening_balance = Some(balance.amount);
    }

    if let Some(field) = fields.find("62") {
        let balance = parse_balance(field, at)?;
        row.closing_balance_kind = kind(field.tag);
        row.closing_balance_dc = Some(balance.dc);
        row.closing_balance_date = balance.date;
        row.closing_balance_currency = Some(balance.currency);
        row.closing_balance = Some(balance.amount);
    }

    if let Some(field) = fields.find("64") {
        let balance = parse_balance(field, at)?;
        row.available_balance_dc = Some(balance.dc);
        row.available_balance_date = balance.date;
        row.available_balance_currency = Some(balance.currency);
        row.available_balance = Some(balance.amount);
    }

    if let Some(field) = fields.find("65") {
        let balance = parse_balance(field, at)?;
        row.forward_available_dc = Some(balance.dc);
        row.forward_available_date = balance.date;
        row.forward_available_currency = Some(balance.currency);
        row.forward_available = Some(balance.amount);
    }

    Ok(row)
}

/// One row from one entry region: the base row, plus the `:61:` the region holds
/// and the `:86:` narrative written under it.
fn row_from_entry(
    open: &Statement,
    index: i64,
    site: &mt::EntrySite,
    source: &str,
) -> Result<Mt940Row, String> {
    let body = &open.msg[open.body.clone()];
    let fields = Fields::parse(&body[site.bytes.clone()]);
    let where_ = format!(
        "{source}:{}",
        mt::at(open.message_line, open.body_line, site.line)
    );
    let Some(field) = fields.find("61") else {
        return Err(format!("{where_}: an entry region with no statement line"));
    };
    let entry = mt::statement_line(&field.value).map_err(|e| format!("{where_}: {e}"))?;

    let mut row = open.base.clone();
    row.entry_index = Some(index);
    row.value_date = entry.value_date;
    row.entry_date = entry.entry_date;
    row.credit_debit = Some(entry.credit_debit);
    row.funds_code = entry.funds_code;
    row.amount = Some(entry.amount);
    row.transaction_type = entry.transaction_type;
    row.transaction_code = entry.transaction_code;
    row.customer_ref = entry.customer_ref;
    row.bank_ref = entry.bank_ref;
    row.supplementary_details = entry.supplementary;
    for narrative in fields.all("86") {
        append_joined(&mut row.narrative, narrative);
    }
    Ok(row)
}

/// The `:86:` fields that belong to the statement rather than to an entry: the
/// ones after the closing balance. An `:86:` before it with no entry above it is
/// a narrative for nothing, and is dropped, which is what this reader has always
/// done.
fn statement_narrative(fields: &Fields<'_>) -> Option<String> {
    let closing_at = fields
        .iter()
        .position(|field| field.tag.starts_with("62"))?;
    let mut out = None;
    for field in fields
        .iter()
        .skip(closing_at + 1)
        .filter(|field| field.tag == "86")
    {
        append_joined(&mut out, &field.value);
    }
    out
}

fn account(tag: &str, value: &str) -> (Option<String>, Option<String>) {
    let mut parts = value.split_whitespace();
    let account = parts.next().and_then(text);
    let account_bic = (tag == "25P")
        .then(|| parts.next().and_then(text))
        .flatten();
    (account, account_bic)
}

fn parse_balance(field: &Field<'_>, at: &At<'_>) -> Result<mt::Balance, String> {
    mt::balance(field.tag, &field.value).map_err(|e| format!("{}: {e}", at(field)))
}

fn kind(tag: &str) -> Option<String> {
    tag.get(2..3).map(str::to_string)
}

fn append_joined(slot: &mut Option<String>, value: &str) {
    let Some(value) = text(value) else {
        return;
    };
    match slot {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(&value);
        }
        None => *slot = Some(value),
    }
}

fn text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}
