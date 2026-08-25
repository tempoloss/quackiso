//! MT101 - Request for Transfer. A corporate asks its bank to make payments on
//! its behalf: the MT predecessor of pain.001, and the one payment-initiation
//! message this repository could read on the ISO 20022 side and not on the MT
//! side.
//!
//! Grain: one row per transaction. An MT101 states a header once and then repeats
//! a transaction sequence, the way a pain.001 states a `PmtInf` once and repeats
//! `CdtTrfTxInf`, so the grain that matches `read_pain001` is the transaction and
//! not the message. The header columns repeat on every row of the message.
//!
//! Two fields may be stated in either sequence. The format allows field 50a and
//! field 52a in the header or in each transaction and not in both, so the columns
//! here are the effective values: what the transaction says, or the header's when
//! the transaction says nothing. That is the ordering customer of the payment
//! either way, which is the question a row is asked.
//!
//! The transaction boundary is an exact `:21:`. It cannot be matched the way the
//! other tags are, because a two-character key matches whatever option letter
//! follows it and this message carries `:21R:` in the header and `:21F:` in a
//! transaction: three different fields whose numbers agree.

use std::collections::VecDeque;
use std::error::Error;
use std::io::BufRead;

use crate::mt::{self, At, Field, Fields};

/// The customer whose account is debited: options F, G and H of field 50a. C and
/// L are the party that instructed the payment, which is a different field.
const ORDERING: &[&str] = &["50F", "50G", "50H"];

#[derive(Debug, Default, Clone)]
pub struct Mt101Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub sender_reference: Option<String>,
    pub customer_reference: Option<String>,
    pub message_index: Option<i64>,
    pub message_total: Option<i64>,
    pub requested_execution_date: Option<i32>,
    pub authorisation: Option<String>,
    pub sending_institution: Option<String>,
    pub instructing_party: Option<String>,
    pub party_option_50: Option<String>,
    pub ordering_customer: Option<String>,
    pub ordering_customer_account: Option<String>,
    pub account_servicing_institution: Option<String>,
    pub account_servicing_institution_account: Option<String>,
    pub tx_ref: Option<String>,
    pub fx_deal_ref: Option<String>,
    pub instruction_codes: Option<String>,
    pub currency: Option<String>,
    pub amount: Option<i128>,
    pub instructed_currency: Option<String>,
    pub instructed_amount: Option<i128>,
    pub exchange_rate: Option<String>,
    pub intermediary_institution: Option<String>,
    pub account_with_institution: Option<String>,
    pub account_with_institution_account: Option<String>,
    pub party_option_59: Option<String>,
    pub beneficiary: Option<String>,
    pub beneficiary_account: Option<String>,
    pub remittance_info: Option<String>,
    pub details_of_charges: Option<String>,
    pub charges_account: Option<String>,
    pub regulatory_reporting: Option<String>,
    pub source_file: Option<String>,
}

pub struct Mt101Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
    /// The rows of the message being read. One FIN message is capped at 10,000
    /// characters by the network, so this is bounded by the message the reader
    /// already holds rather than by the file.
    queue: VecDeque<Mt101Row>,
}

impl<R: BufRead> Mt101Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt101Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
            queue: VecDeque::new(),
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt101Row>, Box<dyn Error>> {
        loop {
            if let Some(row) = self.queue.pop_front() {
                return Ok(Some(row));
            }
            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT101 message found - is this a SWIFT MT101 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_at(&msg, 4).unwrap_or((msg.as_str(), 1));
            let fields = Fields::parse(body);
            if !mt::claims(&msg, &fields, "101") {
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
            for row in rows_from_message(&msg, &fields, &self.source, &at)? {
                self.queue.push_back(row);
            }
        }
    }
}

pub fn rows_from_message(
    msg: &str,
    fields: &Fields<'_>,
    source: &str,
    at: &At<'_>,
) -> Result<Vec<Mt101Row>, String> {
    let spans = mt::sequences(fields, "21");
    let head = 0..spans.first().map(|s| s.start).unwrap_or(fields.len());

    let (message_index, message_total) = match fields.find_in(head.clone(), "28D") {
        Some(field) => mt::index_total(&field.value),
        None => (None, None),
    };
    let requested_execution_date = fields
        .find_in(head.clone(), "30")
        .and_then(|field| mt::date2(field.value.trim()));

    // Field 50a is two different fields sharing a number: C and L name the party
    // instructing the bank, F, G and H name the customer whose account is debited.
    // Only the option letter tells them apart, and both may be in one sequence.
    let template = Mt101Row {
        direction: mt::direction(msg).map(|v| v.to_string()),
        message_type: mt::message_type(msg).map(|v| v.to_string()),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: mt::user_header_field(msg, "121").map(|v| v.to_string()),
        validation_flag: mt::user_header_field(msg, "119").map(|v| v.to_string()),
        mur: mt::user_header_field(msg, "108").map(|v| v.to_string()),
        sender_reference: mt::text(fields.find_in(head.clone(), "20")),
        customer_reference: mt::text(fields.find_in(head.clone(), "21R")),
        message_index,
        message_total,
        requested_execution_date,
        authorisation: mt::text(fields.find_in(head.clone(), "25")),
        sending_institution: mt::party_in(fields, head.clone(), "51A").0,
        source_file: Some(source.to_string()),
        ..Mt101Row::default()
    };

    let head_instructing = mt::instructing_party(fields, head.clone());
    let head_ordering = mt::customer(fields, head.clone(), ORDERING);
    let head_servicing = mt::party_in(fields, head.clone(), "52");

    // A message with no transaction sequence is not a request for transfer. It is
    // reported as one row of header, because a caller globbing a folder needs the
    // message to appear rather than to vanish.
    if spans.is_empty() {
        return Ok(vec![Mt101Row {
            instructing_party: head_instructing,
            party_option_50: head_ordering.0,
            ordering_customer: head_ordering.1,
            ordering_customer_account: head_ordering.2,
            account_servicing_institution: head_servicing.0,
            account_servicing_institution_account: head_servicing.1,
            ..template
        }]);
    }

    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let (instructed_currency, instructed_amount) =
            mt::ccy_amount_in(fields, span.clone(), "33B", at)?;
        let (currency, amount) = mt::ccy_amount_in(fields, span.clone(), "32B", at)?;
        let instructing = mt::instructing_party(fields, span.clone()).or(head_instructing.clone());
        let (option, name, account) = mt::customer(fields, span.clone(), ORDERING);
        let (option, ordering, ordering_account) = match name.is_some() || account.is_some() {
            true => (option, name, account),
            false => head_ordering.clone(),
        };
        let servicing = match mt::party_in(fields, span.clone(), "52") {
            (None, None) => head_servicing.clone(),
            found => found,
        };
        let (account_with_institution, account_with_institution_account) =
            mt::party_in(fields, span.clone(), "57");
        let (party_option_59, beneficiary, beneficiary_account) =
            mt::party_with_option(fields, span.clone(), "59");

        out.push(Mt101Row {
            tx_ref: mt::text(fields.find_in(span.clone(), "21")),
            fx_deal_ref: mt::text(fields.find_in(span.clone(), "21F")),
            instruction_codes: mt::joined(fields.all_in(span.clone(), "23E")),
            currency,
            amount,
            instructed_currency,
            instructed_amount,
            exchange_rate: mt::text(fields.find_in(span.clone(), "36")),
            instructing_party: instructing,
            party_option_50: option,
            ordering_customer: ordering,
            ordering_customer_account: ordering_account,
            account_servicing_institution: servicing.0,
            account_servicing_institution_account: servicing.1,
            intermediary_institution: mt::party_in(fields, span.clone(), "56").0,
            account_with_institution,
            account_with_institution_account,
            party_option_59,
            beneficiary,
            beneficiary_account,
            remittance_info: mt::text(fields.find_in(span.clone(), "70")),
            details_of_charges: mt::text(fields.find_in(span.clone(), "71A")),
            charges_account: mt::text(fields.find_in(span.clone(), "25A")),
            regulatory_reporting: mt::text(fields.find_in(span.clone(), "77B")),
            ..template.clone()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rows(text: &str) -> Vec<Mt101Row> {
        let mut stream = Mt101Stream::new(Cursor::new(text.as_bytes()), "test.fin");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            out.push(row);
        }
        out
    }

    fn message(body: &str) -> String {
        format!("{{1:F01NWBKGB2LAXXX0000000000}}{{2:I101BARCGB22XXXXN}}{{4:\n{body}\n-}}")
    }

    /// The header is stated once and belongs to every transaction under it, the
    /// way a pain.001 `PmtInf` belongs to every `CdtTrfTxInf` inside it.
    #[test]
    fn a_header_is_read_once_and_repeats_on_every_transaction() {
        let rows = rows(&message(
            ":20:REF-1\n:28D:2/7\n:30:260901\n:50H:/ACCT-1\nPAYER LTD\n\
             :21:TX-1\n:32B:EUR100,\n:59:/B1\nPAYEE ONE\n\
             :21:TX-2\n:32B:EUR200,50\n:59:/B2\nPAYEE TWO",
        ));
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.sender_reference.as_deref(), Some("REF-1"));
            assert_eq!((row.message_index, row.message_total), (Some(2), Some(7)));
            assert_eq!(
                row.requested_execution_date,
                Some(mt::date2("260901").unwrap())
            );
            assert_eq!(row.ordering_customer.as_deref(), Some("PAYER LTD"));
        }
        assert_eq!(
            rows.iter()
                .map(|r| (r.tx_ref.as_deref().unwrap(), r.amount.unwrap()))
                .collect::<Vec<_>>(),
            [("TX-1", 10_000_000), ("TX-2", 20_050_000)]
        );
    }

    /// Three fields whose numbers agree and whose meanings do not: `:21R:` is the
    /// customer's reference for the whole message, `:21:` opens a transaction, and
    /// `:21F:` is an FX deal inside one. A two-character key matches all three, so
    /// the transaction boundary is the exact tag.
    #[test]
    fn the_three_reference_fields_are_not_confused_for_each_other() {
        let rows = rows(&message(
            ":20:REF-1\n:21R:CUSTOMER-WHOLE-MESSAGE\n:28D:1/1\n:30:260901\n\
             :50H:/ACCT-1\nPAYER LTD\n\
             :21:TX-1\n:21F:FX-88\n:32B:EUR100,\n:59:/B1\nPAYEE ONE",
        ));
        assert_eq!(rows.len(), 1, "one transaction, not three");
        assert_eq!(
            rows[0].customer_reference.as_deref(),
            Some("CUSTOMER-WHOLE-MESSAGE")
        );
        assert_eq!(rows[0].tx_ref.as_deref(), Some("TX-1"));
        assert_eq!(rows[0].fx_deal_ref.as_deref(), Some("FX-88"));
    }

    /// Field 50a and field 52a may be stated in the header or in each transaction
    /// and not in both, so a row carries the effective party: its own where it has
    /// one, the header's otherwise. Reporting the header value beside an
    /// overriding one would say the payment came from two accounts.
    #[test]
    fn a_transaction_party_overrides_the_header_and_is_inherited_otherwise() {
        let rows = rows(&message(
            ":20:REF-1\n:28D:1/1\n:30:260901\n:50H:/HEADER-ACCT\nHEADER PAYER\n\
             :52A:HEADERBICXXX\n\
             :21:TX-1\n:32B:EUR100,\n:59:/B1\nPAYEE ONE\n\
             :21:TX-2\n:32B:EUR200,\n:50H:/OWN-ACCT\nOWN PAYER\n:52A:OWNBICXXXXXX\n\
             :59:/B2\nPAYEE TWO",
        ));
        assert_eq!(
            rows.iter()
                .map(|r| (
                    r.ordering_customer.as_deref().unwrap(),
                    r.ordering_customer_account.as_deref().unwrap(),
                    r.account_servicing_institution.as_deref().unwrap(),
                ))
                .collect::<Vec<_>>(),
            [
                ("HEADER PAYER", "HEADER-ACCT", "HEADERBICXXX"),
                ("OWN PAYER", "OWN-ACCT", "OWNBICXXXXXX"),
            ]
        );
    }

    /// Options C and L of field 50a name whoever instructed the bank, F, G and H
    /// the customer whose account pays. Both may be in one sequence, and reading
    /// either into the other's column would name the wrong party as the debtor.
    #[test]
    fn the_instructing_party_and_the_ordering_customer_share_a_field_number() {
        let rows = rows(&message(
            ":20:REF-1\n:28D:1/1\n:30:260901\n\
             :50L:TREASURY DESK\n:50G:/PAYER-ACCT\nPAYRBICXXXXX\n\
             :21:TX-1\n:32B:EUR100,\n:59:/B1\nPAYEE ONE",
        ));
        assert_eq!(rows[0].instructing_party.as_deref(), Some("TREASURY DESK"));
        assert_eq!(rows[0].party_option_50.as_deref(), Some("G"));
        assert_eq!(rows[0].ordering_customer.as_deref(), Some("PAYRBICXXXXX"));
        assert_eq!(
            rows[0].ordering_customer_account.as_deref(),
            Some("PAYER-ACCT")
        );
    }

    /// A request with no transaction sequence is malformed, and it still has to
    /// appear: a caller globbing a folder is counting messages, and a file that
    /// silently contributes nothing is the failure ADR 0007 decided against on
    /// the pacs.028 side. The same reasoning, so no second ADR.
    #[test]
    fn a_request_with_no_transaction_is_still_a_row() {
        let rows = rows(&message(
            ":20:REF-1\n:28D:1/1\n:30:260901\n:50H:/A\nPAYER LTD",
        ));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tx_ref, None);
        assert_eq!(rows[0].ordering_customer.as_deref(), Some("PAYER LTD"));
    }

    /// A file of some other MT type is an error and not an empty table, the way it
    /// is for every reader here.
    #[test]
    fn another_message_type_is_refused_by_name() {
        let mt103 = "{1:F01NWBKGB2LAXXX0000000000}{2:I103BARCGB22XXXXN}{4:\n\
                     :20:REF-1\n:23B:CRED\n:32A:260819EUR1,\n-}";
        let mut stream = Mt101Stream::new(Cursor::new(mt103.as_bytes()), "test.fin");
        let err = stream.next_row().expect_err("an MT103 is not an MT101");
        assert!(err.to_string().contains("no MT101 message found"));
    }
}
