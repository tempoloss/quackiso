//! MT103 - Single Customer Credit Transfer. Customer credit transfers sent over
//! SWIFT FIN, usually the MT predecessor to an interbank pacs.008.
//!
//! Grain: one row per MT103 message.

use std::error::Error;
use std::io::BufRead;

use crate::mt::{self, Field, Fields};

/// Turns a field into the `path:line` prefix its errors carry.
type At<'a> = dyn Fn(&Field<'_>) -> String + 'a;

#[derive(Debug, Default, Clone)]
pub struct Mt103Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub tx_ref: Option<String>,
    pub time_indications: Option<String>,
    pub bank_operation_code: Option<String>,
    pub instruction_codes: Option<String>,
    pub transaction_type_code: Option<String>,
    pub value_date: Option<i32>,
    pub currency: Option<String>,
    pub amount: Option<i128>,
    pub instructed_currency: Option<String>,
    pub instructed_amount: Option<i128>,
    pub exchange_rate: Option<String>,
    pub party_option_50: Option<String>,
    pub ordering_customer: Option<String>,
    pub ordering_customer_account: Option<String>,
    pub sending_institution: Option<String>,
    pub ordering_institution: Option<String>,
    pub ordering_institution_account: Option<String>,
    pub senders_correspondent: Option<String>,
    pub senders_correspondent_account: Option<String>,
    pub receivers_correspondent: Option<String>,
    pub third_reimbursement_institution: Option<String>,
    pub intermediary_institution: Option<String>,
    pub account_with_institution: Option<String>,
    pub account_with_institution_account: Option<String>,
    pub party_option_59: Option<String>,
    pub beneficiary: Option<String>,
    pub beneficiary_account: Option<String>,
    pub remittance_info: Option<String>,
    pub details_of_charges: Option<String>,
    pub sender_charges: Option<i128>,
    pub sender_charges_currency: Option<String>,
    pub receiver_charges: Option<i128>,
    pub receiver_charges_currency: Option<String>,
    pub sender_to_receiver_info: Option<String>,
    pub regulatory_reporting: Option<String>,
    pub source_file: Option<String>,
}

pub fn row_from_message(
    msg: &str,
    fields: &Fields<'_>,
    source: &str,
    at: &At<'_>,
) -> Result<Mt103Row, String> {
    let (value_date, currency, amount) = date_ccy_amount(fields, at)?;
    let (instructed_currency, instructed_amount) = ccy_amount(fields, "33B", at)?;
    let (sender_charges_currency, sender_charges) = ccy_amount(fields, "71F", at)?;
    let (receiver_charges_currency, receiver_charges) = ccy_amount(fields, "71G", at)?;

    let (party_option_50, ordering_customer, ordering_customer_account) =
        party_with_option(fields, "50");
    let (ordering_institution, ordering_institution_account) = party(fields, "52");
    let (senders_correspondent, senders_correspondent_account) = party(fields, "53");
    let (account_with_institution, account_with_institution_account) = party(fields, "57");
    let (party_option_59, beneficiary, beneficiary_account) = party_with_option(fields, "59");

    Ok(Mt103Row {
        direction: mt::direction(msg).map(|v| v.to_string()),
        message_type: mt::message_type(msg).map(|v| v.to_string()),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: mt::user_header_field(msg, "121").map(|v| v.to_string()),
        validation_flag: mt::user_header_field(msg, "119").map(|v| v.to_string()),
        mur: mt::user_header_field(msg, "108").map(|v| v.to_string()),
        tx_ref: text(fields.value("20")),
        time_indications: joined(fields.all("13C")),
        bank_operation_code: text(fields.value("23B")),
        instruction_codes: joined(fields.all("23E")),
        transaction_type_code: text(fields.value("26T")),
        value_date,
        currency,
        amount,
        instructed_currency,
        instructed_amount,
        exchange_rate: text(fields.value("36")),
        party_option_50,
        ordering_customer,
        ordering_customer_account,
        sending_institution: party(fields, "51A").0,
        ordering_institution,
        ordering_institution_account,
        senders_correspondent,
        senders_correspondent_account,
        receivers_correspondent: party(fields, "54").0,
        third_reimbursement_institution: party(fields, "55").0,
        intermediary_institution: party(fields, "56").0,
        account_with_institution,
        account_with_institution_account,
        party_option_59,
        beneficiary,
        beneficiary_account,
        remittance_info: text(fields.value("70")),
        details_of_charges: text(fields.value("71A")),
        sender_charges,
        sender_charges_currency,
        receiver_charges,
        receiver_charges_currency,
        sender_to_receiver_info: text(fields.value("72")),
        regulatory_reporting: text(fields.value("77B")),
        source_file: Some(source.to_string()),
    })
}

pub struct Mt103Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
}

impl<R: BufRead> Mt103Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt103Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt103Row>, Box<dyn Error>> {
        loop {
            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT103 message found - is this a SWIFT MT103 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_at(&msg, 4).unwrap_or((msg.as_str(), 1));
            let fields = Fields::parse(body);
            if !mt::claims(&msg, &fields, "103") {
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

fn date_ccy_amount(fields: &Fields<'_>, at: &At<'_>) -> Result<mt::DateCcyAmount, String> {
    match fields.find("32A") {
        Some(field) => {
            let (date, currency, amount) =
                mt::date_ccy_amount(&field.value).map_err(|e| format!("{}: {e}", at(field)))?;
            Ok((date, Some(currency), Some(amount)))
        }
        None => Ok((None, None, None)),
    }
}

fn ccy_amount(
    fields: &Fields<'_>,
    tag: &str,
    at: &At<'_>,
) -> Result<(Option<String>, Option<i128>), String> {
    match fields.find(tag) {
        Some(field) => {
            let (currency, amount) =
                mt::ccy_amount(tag, &field.value).map_err(|e| format!("{}: {e}", at(field)))?;
            Ok((Some(currency), Some(amount)))
        }
        None => Ok((None, None)),
    }
}

fn party_with_option(
    fields: &Fields<'_>,
    key: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(field) = fields.find(key) else {
        return (None, None, None);
    };
    let option = field
        .tag
        .as_bytes()
        .get(2)
        .map(|b| char::from(*b).to_string());
    let (identifier, account, _) = mt::party(field.tag, &field.value);
    (option, identifier, account)
}

fn party(fields: &Fields<'_>, key: &str) -> (Option<String>, Option<String>) {
    let Some(field) = fields.find(key) else {
        return (None, None);
    };
    let (identifier, account, _) = mt::party(field.tag, &field.value);
    (identifier, account)
}

fn joined(values: Vec<&str>) -> Option<String> {
    (!values.is_empty()).then(|| values.join("\n"))
}

fn text(value: Option<&str>) -> Option<String> {
    value.map(|v| v.to_string())
}
