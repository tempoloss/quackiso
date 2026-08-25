//! MT104 - Direct Debit and Request for Debit Transfer. A creditor collects from
//! debtors through its bank: the MT side of pain.008, and the collection half of
//! the payment lifecycle this repository read only on the ISO 20022 side.
//!
//! The direction is the thing to hold on to. An MT101 and an MT103 pay someone,
//! so field 59 is the party being paid. An MT104 collects, so field 59 is the
//! party being debited and field 50a is the creditor doing the collecting: the
//! same tags as a credit transfer, at opposite ends of the payment. That is why
//! `audit_addresses` reads roles against the message number and not the tag
//! alone.
//!
//! Grain: one row per transaction. The header repeats on every row, the way it
//! does in `read_mt101` and `read_pain001`.
//!
//! The transaction boundary is an exact `:21:`, for the reason it is in an MT101:
//! this message also carries `:21R:` in its header and `:21C:` and `:21D:` in a
//! transaction, and a two-character key would open a sequence on any of them.
//!
//! A third sequence is the wrinkle an MT101 does not have. Sequence C states the
//! settlement total for the whole batch, and it sits after the last transaction
//! with a `:32B:` of its own -- so the tail the transaction split hands back holds
//! the last transaction and then the batch total. It is divided at the first
//! `:19:` or the second `:32B:`, whichever comes first: `:32B:` is mandatory in
//! both sequences and `:19:` occurs only in C. Getting this wrong does not fail
//! loudly, it just reports the batch total as one more transaction, and `:71F:`
//! and `:71G:` occur in both sequences too.
//!
//! `:19:` is worth a column rather than a check of its own. The format requires it
//! exactly when the settlement total differs from the sum of the transactions, so
//! reporting both is what lets a caller ask whether a batch adds up.

use std::collections::VecDeque;
use std::error::Error;
use std::io::BufRead;
use std::ops::Range;

use crate::mt::{self, At, Field, Fields};

/// The creditor collecting the money: options A and K of field 50a. C and L are
/// the party that instructed the collection, which is a different field.
const CREDITOR: &[&str] = &["50A", "50K"];

#[derive(Debug, Default, Clone)]
pub struct Mt104Row {
    pub direction: Option<String>,
    pub message_type: Option<String>,
    pub sender_bic: Option<String>,
    pub receiver_bic: Option<String>,
    pub uetr: Option<String>,
    pub validation_flag: Option<String>,
    pub mur: Option<String>,
    pub sender_reference: Option<String>,
    pub customer_reference: Option<String>,
    pub registration_reference: Option<String>,
    pub requested_execution_date: Option<i32>,
    pub sending_institution: Option<String>,
    pub instructing_party: Option<String>,
    pub party_option_50: Option<String>,
    pub creditor: Option<String>,
    pub creditor_account: Option<String>,
    pub creditor_bank: Option<String>,
    pub creditor_bank_account: Option<String>,
    pub transaction_type_code: Option<String>,
    pub details_of_charges: Option<String>,
    pub regulatory_reporting: Option<String>,
    pub sender_to_receiver: Option<String>,
    pub tx_ref: Option<String>,
    pub instruction_codes: Option<String>,
    pub mandate_reference: Option<String>,
    pub direct_debit_reference: Option<String>,
    pub currency: Option<String>,
    pub amount: Option<i128>,
    pub instructed_currency: Option<String>,
    pub instructed_amount: Option<i128>,
    pub exchange_rate: Option<String>,
    pub debtor_bank: Option<String>,
    pub debtor_bank_account: Option<String>,
    pub party_option_59: Option<String>,
    pub debtor: Option<String>,
    pub debtor_account: Option<String>,
    pub remittance_info: Option<String>,
    pub senders_charges: Option<String>,
    pub receivers_charges: Option<String>,
    pub settlement_currency: Option<String>,
    pub settlement_amount: Option<i128>,
    pub sum_of_amounts: Option<i128>,
    pub sum_senders_charges: Option<String>,
    pub sum_receivers_charges: Option<String>,
    pub senders_correspondent: Option<String>,
    pub source_file: Option<String>,
}

pub struct Mt104Stream<R: BufRead> {
    reader: mt::MtReader<R>,
    source: String,
    saw_my_type: bool,
    /// The rows of the message being read. One FIN message is capped at 10,000
    /// characters by the network, so this is bounded by the message the reader
    /// already holds rather than by the file.
    queue: VecDeque<Mt104Row>,
}

impl<R: BufRead> Mt104Stream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        Mt104Stream {
            reader: mt::MtReader::new(reader, source),
            source: source.to_string(),
            saw_my_type: false,
            queue: VecDeque::new(),
        }
    }

    pub fn next_row(&mut self) -> Result<Option<Mt104Row>, Box<dyn Error>> {
        loop {
            if let Some(row) = self.queue.pop_front() {
                return Ok(Some(row));
            }
            let Some(msg) = self.reader.next_message()? else {
                return if self.saw_my_type {
                    Ok(None)
                } else {
                    Err(format!(
                        "{}: no MT104 message found - is this a SWIFT MT104 file?",
                        self.source
                    )
                    .into())
                };
            };

            let message_line = self.reader.line();
            let (body, body_line) = mt::block_at(&msg, 4).unwrap_or((msg.as_str(), 1));
            let fields = Fields::parse(body);
            if !mt::claims(&msg, &fields, "104") {
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

/// Where the settlement sequence starts inside the last transaction's span, if it
/// is there at all.
///
/// The transaction split runs the last span to the end of the message, so a batch
/// total lands inside it. `:32B:` cannot tell the two apart on its own -- it is
/// mandatory in a transaction and mandatory in the settlement -- so the opener is
/// whichever comes first of a `:19:`, which only the settlement has, and a second
/// `:32B:`, which only the settlement can be.
fn settlement_start(fields: &Fields<'_>, tail: Range<usize>) -> Option<usize> {
    let amounts: Vec<usize> = fields
        .iter()
        .enumerate()
        .skip(tail.start)
        .take(tail.len())
        .filter(|(_, field)| field.tag == "32B")
        .map(|(index, _)| index)
        .collect();
    let sum = fields
        .iter()
        .enumerate()
        .skip(tail.start)
        .take(tail.len())
        .find(|(_, field)| field.tag == "19")
        .map(|(index, _)| index);
    match (sum, amounts.get(1)) {
        (Some(a), Some(b)) => Some(a.min(*b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(*b),
        (None, None) => None,
    }
}

pub fn rows_from_message(
    msg: &str,
    fields: &Fields<'_>,
    source: &str,
    at: &At<'_>,
) -> Result<Vec<Mt104Row>, String> {
    let mut spans = mt::sequences(fields, "21");
    let settlement = match spans.last().cloned() {
        Some(tail) => match settlement_start(fields, tail.clone()) {
            Some(start) => {
                *spans.last_mut().expect("a last span, matched above") = tail.start..start;
                Some(start..tail.end)
            }
            None => None,
        },
        None => None,
    };
    let head = 0..spans.first().map(|s| s.start).unwrap_or(fields.len());
    let settle = settlement.unwrap_or(fields.len()..fields.len());

    let (settlement_currency, settlement_amount) =
        mt::ccy_amount_in(fields, settle.clone(), "32B", at)?;
    let sum_of_amounts = match fields.find_in(settle.clone(), "19") {
        Some(field) => {
            Some(mt::amount(field.value.trim()).map_err(|e| format!("{}: {e}", at(field)))?)
        }
        None => None,
    };

    let template = Mt104Row {
        direction: mt::direction(msg).map(|v| v.to_string()),
        message_type: mt::message_type(msg).map(|v| v.to_string()),
        sender_bic: mt::sender_bic(msg),
        receiver_bic: mt::receiver_bic(msg),
        uetr: mt::user_header_field(msg, "121").map(|v| v.to_string()),
        validation_flag: mt::user_header_field(msg, "119").map(|v| v.to_string()),
        mur: mt::user_header_field(msg, "108").map(|v| v.to_string()),
        sender_reference: mt::text(fields.find_in(head.clone(), "20")),
        customer_reference: mt::text(fields.find_in(head.clone(), "21R")),
        requested_execution_date: fields
            .find_in(head.clone(), "30")
            .and_then(|field| mt::date2(field.value.trim())),
        sending_institution: mt::party_in(fields, head.clone(), "51A").0,
        sender_to_receiver: mt::text(fields.find_in(head.clone(), "72")),
        settlement_currency,
        settlement_amount,
        sum_of_amounts,
        sum_senders_charges: mt::text(fields.find_in(settle.clone(), "71F")),
        sum_receivers_charges: mt::text(fields.find_in(settle.clone(), "71G")),
        senders_correspondent: mt::party_in(fields, settle.clone(), "53").0,
        source_file: Some(source.to_string()),
        ..Mt104Row::default()
    };

    // Seven fields may be stated once in the header or once in every transaction,
    // and not in both. The columns are the effective values: what the transaction
    // says, or the header's when it says nothing.
    let head_instructing = mt::instructing_party(fields, head.clone());
    let head_creditor = mt::customer(fields, head.clone(), CREDITOR);
    let head_bank = mt::party_in(fields, head.clone(), "52");
    let head_type = mt::text(fields.find_in(head.clone(), "26T"));
    let head_charges = mt::text(fields.find_in(head.clone(), "71A"));
    let head_regulatory = mt::text(fields.find_in(head.clone(), "77B"));
    let head_registration = mt::text(fields.find_in(head.clone(), "21E"));

    // A message with no transaction sequence is not a direct debit. It is reported
    // as one row of header, because a caller globbing a folder needs the message to
    // appear rather than to vanish.
    if spans.is_empty() {
        return Ok(vec![Mt104Row {
            instructing_party: head_instructing,
            party_option_50: head_creditor.0,
            creditor: head_creditor.1,
            creditor_account: head_creditor.2,
            creditor_bank: head_bank.0,
            creditor_bank_account: head_bank.1,
            transaction_type_code: head_type,
            details_of_charges: head_charges,
            regulatory_reporting: head_regulatory,
            registration_reference: head_registration,
            ..template
        }]);
    }

    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let (instructed_currency, instructed_amount) =
            mt::ccy_amount_in(fields, span.clone(), "33B", at)?;
        let (currency, amount) = mt::ccy_amount_in(fields, span.clone(), "32B", at)?;
        let (option, name, account) = mt::customer(fields, span.clone(), CREDITOR);
        let (party_option_50, creditor, creditor_account) =
            match name.is_some() || account.is_some() {
                true => (option, name, account),
                false => head_creditor.clone(),
            };
        let bank = match mt::party_in(fields, span.clone(), "52") {
            (None, None) => head_bank.clone(),
            found => found,
        };
        let (debtor_bank, debtor_bank_account) = mt::party_in(fields, span.clone(), "57");
        let (party_option_59, debtor, debtor_account) =
            mt::party_with_option(fields, span.clone(), "59");

        out.push(Mt104Row {
            tx_ref: mt::text(fields.find_in(span.clone(), "21")),
            instruction_codes: mt::joined(fields.all_in(span.clone(), "23E")),
            mandate_reference: mt::text(fields.find_in(span.clone(), "21C")),
            direct_debit_reference: mt::text(fields.find_in(span.clone(), "21D")),
            registration_reference: mt::text(fields.find_in(span.clone(), "21E"))
                .or(head_registration.clone()),
            currency,
            amount,
            instructed_currency,
            instructed_amount,
            exchange_rate: mt::text(fields.find_in(span.clone(), "36")),
            instructing_party: mt::instructing_party(fields, span.clone())
                .or(head_instructing.clone()),
            party_option_50,
            creditor,
            creditor_account,
            creditor_bank: bank.0,
            creditor_bank_account: bank.1,
            debtor_bank,
            debtor_bank_account,
            party_option_59,
            debtor,
            debtor_account,
            remittance_info: mt::text(fields.find_in(span.clone(), "70")),
            transaction_type_code: mt::text(fields.find_in(span.clone(), "26T"))
                .or(head_type.clone()),
            details_of_charges: mt::text(fields.find_in(span.clone(), "71A"))
                .or(head_charges.clone()),
            senders_charges: mt::text(fields.find_in(span.clone(), "71F")),
            receivers_charges: mt::text(fields.find_in(span.clone(), "71G")),
            regulatory_reporting: mt::text(fields.find_in(span.clone(), "77B"))
                .or(head_regulatory.clone()),
            ..template.clone()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rows(text: &str) -> Vec<Mt104Row> {
        let mut stream = Mt104Stream::new(Cursor::new(text.as_bytes()), "test.fin");
        let mut out = Vec::new();
        while let Some(row) = stream.next_row().expect("the fixture parses") {
            out.push(row);
        }
        out
    }

    fn message(body: &str) -> String {
        format!("{{1:F01NWBKGB2LAXXX0000000000}}{{2:I104BARCGB22XXXXN}}{{4:\n{body}\n-}}")
    }

    /// The settlement sequence is the batch total, not one more collection. It
    /// opens with a `:32B:` of its own after the last transaction, so a reader that
    /// splits on `:21:` alone reports it as a transaction with no reference and a
    /// balance that looks like money someone owes.
    #[test]
    fn the_settlement_total_is_not_a_transaction() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n:50K:/C-1\nCREDITOR LTD\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n\
             :21:TX-2\n:32B:USD350,\n:59:/D2\nDEBTOR TWO\n\
             :32B:USD605,\n:19:600,\n:71F:USD5,\n:53A:BARCGB22XXX",
        ));
        assert_eq!(rows.len(), 2, "two collections and a batch total");
        assert_eq!(
            rows.iter().map(|r| r.amount.unwrap()).sum::<i128>(),
            60_000_000,
            "the transactions add to 600, not 1205"
        );
        for row in &rows {
            assert_eq!(row.settlement_amount, Some(60_500_000));
            assert_eq!(row.sum_of_amounts, Some(60_000_000));
            assert_eq!(row.senders_correspondent.as_deref(), Some("BARCGB22XXX"));
        }
    }

    /// `:19:` is present exactly when the settlement total differs from the sum of
    /// the transactions, which is the one thing that makes it worth a column:
    /// reporting both is what lets a caller ask whether a batch adds up.
    #[test]
    fn the_batch_can_be_checked_against_its_transactions() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n\
             :21:TX-2\n:32B:USD350,\n:59:/D2\nDEBTOR TWO\n\
             :32B:USD605,\n:19:600,\n:71F:USD5,",
        ));
        let added: i128 = rows.iter().map(|r| r.amount.unwrap()).sum();
        assert_eq!(added, rows[0].sum_of_amounts.unwrap(), "19 is the sum");
        assert_ne!(
            added,
            rows[0].settlement_amount.unwrap(),
            "and the settlement is not, which is why 19 is on the wire"
        );
    }

    /// `:71F:` and `:71G:` occur in a transaction and in the settlement, so the
    /// two have to be read from their own sequences. Sharing them would put a
    /// batch's total charges on every collection in it.
    #[test]
    fn charges_do_not_cross_between_a_transaction_and_the_batch() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n:71F:USD1,\n\
             :21:TX-2\n:32B:USD350,\n:59:/D2\nDEBTOR TWO\n:71F:USD2,50\n\
             :32B:USD605,\n:19:600,\n:71F:USD9,99",
        ));
        assert_eq!(rows[0].senders_charges.as_deref(), Some("USD1,"));
        assert_eq!(rows[1].senders_charges.as_deref(), Some("USD2,50"));
        for row in &rows {
            assert_eq!(row.sum_senders_charges.as_deref(), Some("USD9,99"));
        }
    }

    /// A direct debit runs the other way. Field 50a is the creditor collecting and
    /// field 59a is the debtor being charged: the same tags an MT101 uses for the
    /// payer and the payee, at the opposite ends of the payment.
    #[test]
    fn fifty_is_the_creditor_and_fifty_nine_is_the_debtor() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n\
             :21:TX-1\n:32B:USD250,\n:50K:/C-1\nCREDITOR LTD\n:59:/D-1\nDEBTOR LTD",
        ));
        assert_eq!(rows[0].creditor.as_deref(), Some("CREDITOR LTD"));
        assert_eq!(rows[0].creditor_account.as_deref(), Some("C-1"));
        assert_eq!(rows[0].debtor.as_deref(), Some("DEBTOR LTD"));
        assert_eq!(rows[0].debtor_account.as_deref(), Some("D-1"));
    }

    /// Four references whose numbers agree and whose meanings do not: `:21R:` is
    /// the creditor's reference for the message, `:21:` opens a collection, and
    /// `:21C:` and `:21D:` are the mandate and the direct-debit reference inside
    /// one. A two-character key matches all four.
    #[test]
    fn the_four_reference_fields_are_not_confused_for_each_other() {
        let rows = rows(&message(
            ":20:B-1\n:21R:CREDITOR-REF\n:30:261201\n\
             :21:TX-1\n:21C:MANDATE-1\n:21D:DD-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE",
        ));
        assert_eq!(rows.len(), 1, "only :21: opens a collection");
        assert_eq!(rows[0].customer_reference.as_deref(), Some("CREDITOR-REF"));
        assert_eq!(rows[0].tx_ref.as_deref(), Some("TX-1"));
        assert_eq!(rows[0].mandate_reference.as_deref(), Some("MANDATE-1"));
        assert_eq!(rows[0].direct_debit_reference.as_deref(), Some("DD-1"));
    }

    /// Seven fields may be stated in the header or in each transaction and not in
    /// both, so a column is the effective value: the transaction's when it has one.
    #[test]
    fn a_transaction_overrides_the_header_it_sits_under() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n:50K:/HEAD\nHEAD CREDITOR\n:52A:BARCGB22XXX\n:71A:SHA\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n\
             :21:TX-2\n:32B:USD350,\n:50K:/OWN\nOWN CREDITOR\n:71A:OUR\n:59:/D2\nDEBTOR TWO",
        ));
        assert_eq!(rows[0].creditor.as_deref(), Some("HEAD CREDITOR"));
        assert_eq!(rows[0].details_of_charges.as_deref(), Some("SHA"));
        assert_eq!(rows[1].creditor.as_deref(), Some("OWN CREDITOR"));
        assert_eq!(rows[1].details_of_charges.as_deref(), Some("OUR"));
        for row in &rows {
            assert_eq!(row.creditor_bank.as_deref(), Some("BARCGB22XXX"));
        }
    }

    /// A message with no collection in it is still reported, because a caller
    /// globbing a folder is counting messages and one that vanishes is worse than
    /// one with empty transaction columns.
    #[test]
    fn a_message_with_no_transaction_is_still_a_row() {
        let rows = rows(&message(":20:B-1\n:30:261201\n:50K:/C-1\nCREDITOR LTD"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sender_reference.as_deref(), Some("B-1"));
        assert_eq!(rows[0].creditor.as_deref(), Some("CREDITOR LTD"));
        assert_eq!(rows[0].tx_ref, None);
    }

    /// Without a settlement sequence the last transaction still ends at the end of
    /// the message, and nothing is taken off it.
    #[test]
    fn a_batch_with_no_settlement_sequence_reads_every_transaction_whole() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n:70:/RFB/ONE\n\
             :21:TX-2\n:32B:USD350,\n:59:/D2\nDEBTOR TWO\n:70:/RFB/TWO",
        ));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].remittance_info.as_deref(), Some("/RFB/TWO"));
        assert_eq!(rows[1].settlement_amount, None);
        assert_eq!(rows[1].sum_of_amounts, None);
    }

    /// A file that is some other MT is not an MT104, and saying so is the reader's
    /// job rather than returning nothing.
    #[test]
    fn another_message_type_is_refused_by_name() {
        let text = "{1:F01NWBKGB2LAXXX0000000000}{2:I103BARCGB22XXXXN}{4:\n:20:X\n-}";
        let mut stream = Mt104Stream::new(Cursor::new(text.as_bytes()), "test.fin");
        let err = stream.next_row().expect_err("not an MT104");
        assert!(err.to_string().contains("no MT104 message found"));
    }
    /// The format omits `:19:` when the settlement equals the sum of the
    /// transactions, so the common shape of a batch has no sum field at all and the
    /// only thing marking the settlement off is that its `:32B:` is the second one
    /// in the tail. Every other test here carries a `:19:`, which would leave the
    /// majority shape unread.
    #[test]
    fn a_settlement_with_no_sum_field_is_still_found() {
        let rows = rows(&message(
            ":20:B-1\n:30:261201\n\
             :21:TX-1\n:32B:USD250,\n:59:/D1\nDEBTOR ONE\n\
             :21:TX-2\n:32B:USD350,\n:59:/D2\nDEBTOR TWO\n\
             :32B:USD600,\n:53A:BARCGB22XXX",
        ));
        assert_eq!(rows.len(), 2, "the batch total is not a third collection");
        assert_eq!(
            rows[1].amount,
            Some(35_000_000),
            "the last one keeps its own"
        );
        for row in &rows {
            assert_eq!(row.settlement_amount, Some(60_000_000));
            assert_eq!(row.sum_of_amounts, None, "omitted because the two agree");
            assert_eq!(row.senders_correspondent.as_deref(), Some("BARCGB22XXX"));
        }
    }
}
