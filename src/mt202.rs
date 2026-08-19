//! MT202 - General Financial Institution Transfer, including MT202COV when the
//! user header carries `{119:COV}`.
//!
//! Grain: one row per MT202/MT202COV message.

use std::error::Error;
use std::io::BufRead;
use std::ops::Range;

use crate::mt::{self, Field, Fields};

/// Turns a field into the `path:line` prefix its errors carry.
type At<'a> = dyn Fn(&Field<'_>) -> String + 'a;

#[derive(Debug, Default, Clone)]
pub struct Mt202Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub variant: Option<String>,
    pub tx_ref: Option<String>,
    pub related_ref: Option<String>,
    pub time_indications: Option<String>,
    pub value_date: Option<i32>,
    pub currency: Option<String>,
    pub amount: Option<i128>,
    pub ordering_institution: Option<String>,
    pub ordering_institution_account: Option<String>,
    pub senders_correspondent: Option<String>,
    pub senders_correspondent_account: Option<String>,
    pub receivers_correspondent: Option<String>,
    pub intermediary_institution: Option<String>,
    pub account_with_institution: Option<String>,
    pub account_with_institution_account: Option<String>,
    pub beneficiary_institution: Option<String>,
    pub beneficiary_institution_account: Option<String>,
    pub sender_to_receiver_info: Option<String>,
    pub cov_ordering_customer: Option<String>,
    pub cov_ordering_customer_account: Option<String>,
    pub cov_ordering_institution: Option<String>,
    pub cov_intermediary_institution: Option<String>,
    pub cov_account_with_institution: Option<String>,
    pub cov_beneficiary: Option<String>,
    pub cov_beneficiary_account: Option<String>,
    pub cov_remittance_info: Option<String>,
    pub cov_sender_to_receiver_info: Option<String>,
    pub cov_instructed_currency: Option<String>,
    pub cov_instructed_amount: Option<i128>,
    pub source_file: Option<String>,
}

pub fn row_from_message(
    msg: &str,
    fields: &Fields<'_>,
    source: &str,
    at: &At<'_>,
) -> Result<Mt202Row, String> {
    let len = fields.len();
    let b_start = fields.position(&["50"]).unwrap_or(len);

    let validation_flag = text(mt::user_header_field(msg, "119"));
    let variant = (validation_flag.as_deref() == Some("COV")).then(|| "COV".to_string());
    let (value_date, currency, amount) = date_ccy_amount_in(fields, 0..b_start, at)?;
    let (ordering_institution, ordering_institution_account) =
        party_id_account(fields, 0..b_start, "52");
    let (senders_correspondent, senders_correspondent_account) =
        party_id_account(fields, 0..b_start, "53");
    let receivers_correspondent = party_id(fields, 0..b_start, "54");
    let intermediary_institution = party_id(fields, 0..b_start, "56");
    let (account_with_institution, account_with_institution_account) =
        party_id_account(fields, 0..b_start, "57");
    let (beneficiary_institution, beneficiary_institution_account) =
        party_id_account(fields, 0..b_start, "58");

    let (cov_ordering_customer, cov_ordering_customer_account) =
        party_id_account(fields, b_start..len, "50");
    let cov_ordering_institution = party_id(fields, b_start..len, "52");
    let cov_intermediary_institution = party_id(fields, b_start..len, "56");
    let cov_account_with_institution = party_id(fields, b_start..len, "57");
    let (cov_beneficiary, cov_beneficiary_account) = party_id_account(fields, b_start..len, "59");
    let (cov_instructed_currency, cov_instructed_amount) =
        ccy_amount_in(fields, b_start..len, "33B", at)?;

    Ok(Mt202Row {
        direction: mt::direction(msg).map(str::to_string),
        message_type: mt::message_type(msg).map(str::to_string),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: text(mt::user_header_field(msg, "121")),
        validation_flag,
        mur: text(mt::user_header_field(msg, "108")),
        variant,
        tx_ref: text_in(fields, 0..b_start, "20"),
        related_ref: text_in(fields, 0..b_start, "21"),
        time_indications: join_in(fields, 0..b_start, "13C"),
        value_date,
        currency,
        amount,
        ordering_institution,
        ordering_institution_account,
        senders_correspondent,
        senders_correspondent_account,
        receivers_correspondent,
        intermediary_institution,
        account_with_institution,
        account_with_institution_account,
        beneficiary_institution,
        beneficiary_institution_account,
        sender_to_receiver_info: text_in(fields, 0..b_start, "72"),
        cov_ordering_customer,
        cov_ordering_customer_account,
        cov_ordering_institution,
        cov_intermediary_institution,
        cov_account_with_institution,
        cov_beneficiary,
        cov_beneficiary_account,
        cov_remittance_info: text_in(fields, b_start..len, "70"),
        cov_sender_to_receiver_info: text_in(fields, b_start..len, "72"),
        cov_instructed_currency,
        cov_instructed_amount,
        source_file: Some(source.to_string()),
    })
}

pub struct Mt202Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
}

impl<R: BufRead> Mt202Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt202Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt202Row>, Box<dyn Error>> {
        loop {
            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT202 message found - is this a SWIFT MT202 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_at(&msg, 4).unwrap_or((msg.as_str(), 1));
            let fields = Fields::parse(body);
            if !mt::claims(&msg, &fields, "202") {
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
            return Ok(Some(row_from_message(&msg, &fields, &self.source, &at)?));
        }
    }
}

fn text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn text_in(fields: &Fields<'_>, range: Range<usize>, key: &str) -> Option<String> {
    fields
        .find_in(range, key)
        .and_then(|field| text(Some(&field.value)))
}

fn join_in(fields: &Fields<'_>, range: Range<usize>, key: &str) -> Option<String> {
    let values: Vec<&str> = fields.all_in(range, key);
    (!values.is_empty()).then(|| values.join("\n"))
}

fn date_ccy_amount_in(
    fields: &Fields<'_>,
    range: Range<usize>,
    at: &At<'_>,
) -> Result<mt::DateCcyAmount, String> {
    match fields.find_in(range, "32A") {
        Some(field) => {
            let (date, currency, amount) =
                mt::date_ccy_amount(&field.value).map_err(|e| format!("{}: {e}", at(field)))?;
            Ok((date, Some(currency), Some(amount)))
        }
        None => Ok((None, None, None)),
    }
}

fn ccy_amount_in(
    fields: &Fields<'_>,
    range: Range<usize>,
    key: &str,
    at: &At<'_>,
) -> Result<(Option<String>, Option<i128>), String> {
    match fields.find_in(range, key) {
        Some(field) => {
            let (currency, amount) = mt::ccy_amount(field.tag, &field.value)
                .map_err(|e| format!("{}: {e}", at(field)))?;
            Ok((Some(currency), Some(amount)))
        }
        None => Ok((None, None)),
    }
}

fn party_id(fields: &Fields<'_>, range: Range<usize>, key: &str) -> Option<String> {
    party_id_account(fields, range, key).0
}

fn party_id_account(
    fields: &Fields<'_>,
    range: Range<usize>,
    key: &str,
) -> (Option<String>, Option<String>) {
    fields
        .find_in(range, key)
        .map(|field| {
            let (identifier, account, _) = mt::party(field.tag, &field.value);
            (identifier, account)
        })
        .unwrap_or((None, None))
}
