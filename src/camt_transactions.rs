//! `read_camt_transactions` - every `<TxDtls>` of a camt.052/.053/.054, one row
//! each.
//!
//! `read_iso20022` is one row per `<Ntry>`, which is the grain a bank statement
//! is reconciled at, and it is the wrong grain for the payments inside a batch.
//! An entry of 900 CHF posted as three `<NtryDtls>` of one transaction each has
//! three end-to-end ids, three counterparties and three remittance texts, and an
//! entry row has one column for each. It used to answer with the first
//! transaction's: three payments to three parties reported as one.
//!
//! Grain: one row per `Ntry/NtryDtls/TxDtls`. An entry with no transactions
//! produces no rows here - its money is on the entry row, where the count says
//! how many transactions were under it. No synthetic row is invented from
//! `Btch/NbOfTxs`: a batch that states five transactions and carries none has
//! nothing here to describe, and five empty rows would be five claims nobody
//! made.
//!
//! Nothing falls back. `debtor_name` is `RltdPties/Dbtr`, not the counterparty
//! resolved across both sides; `amount` is `TxDtls/Amt`, not the entry's;
//! `bank_transaction_domain` is the transaction's `BkTxCd`, not the entry's.
//! Entry facts are repeated under `entry_*`, so a query can compare the two
//! instead of being handed one where it asked for the other. That is the whole
//! difference between this and the convenience columns on the entry row, and it
//! is why both exist.
//!
//! Join keys: `source_file`, `statement_index`, `entry_index`,
//! `entry_details_index`, `transaction_index`.

use std::error::Error;
use std::io::BufRead;

use crate::camt::{StatementContext, StatementRecordStream};
use crate::model::{
    money, BankTransactionCode, BatchInformation, Ntry, Party, RltdPties, TxCursor, TxDtls,
};

#[derive(Debug, Default, Clone)]
pub struct TransactionRow {
    pub msg_id: Option<String>,
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub statement_id: Option<String>,
    pub account_iban: Option<String>,
    pub account_currency: Option<String>,
    pub entry_index: Option<i64>,
    pub entry_ref: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub entry_amount: Option<i128>,
    pub entry_currency: Option<String>,
    pub entry_credit_debit: Option<String>,
    pub entry_reversal_indicator: Option<String>,
    pub entry_status: Option<String>,
    pub booking_date: Option<String>,
    pub value_date: Option<String>,
    pub bank_ref: Option<String>,
    pub entry_bank_transaction_domain: Option<String>,
    pub entry_bank_transaction_family: Option<String>,
    pub entry_bank_transaction_subfamily: Option<String>,
    pub entry_bank_transaction_proprietary: Option<String>,
    pub entry_bank_transaction_proprietary_issuer: Option<String>,
    pub entry_details_index: Option<i64>,
    pub transaction_index: Option<i64>,
    pub batch_message_id: Option<String>,
    pub batch_payment_info_id: Option<String>,
    /// A wire count, kept as spelled: it is what the sender said the batch held,
    /// which is not always how many transactions are here.
    pub batch_number_of_transactions: Option<String>,
    pub batch_total_amount: Option<i128>,
    pub batch_total_currency: Option<String>,
    pub batch_credit_debit: Option<String>,
    pub instruction_id: Option<String>,
    pub end_to_end_id: Option<String>,
    pub transaction_id: Option<String>,
    pub uetr: Option<String>,
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub credit_debit: Option<String>,
    pub debtor_name: Option<String>,
    pub debtor_account: Option<String>,
    pub ultimate_debtor_name: Option<String>,
    pub creditor_name: Option<String>,
    pub creditor_account: Option<String>,
    pub ultimate_creditor_name: Option<String>,
    pub bank_transaction_domain: Option<String>,
    pub bank_transaction_family: Option<String>,
    pub bank_transaction_subfamily: Option<String>,
    pub bank_transaction_proprietary: Option<String>,
    pub bank_transaction_proprietary_issuer: Option<String>,
    /// How many supported remittance leaves this transaction states.
    /// `read_camt_remittance` has them.
    pub remittance_count: Option<i64>,
    pub source_file: Option<String>,
}

/// Where the account number of a party sits, when the message states one.
fn account(acct: Option<&crate::model::Acct>) -> Option<String> {
    acct.and_then(|a| a.id.as_ref()).and_then(|id| id.value())
}

#[allow(clippy::too_many_arguments)]
pub fn row_from_transaction(
    ntry: &Ntry,
    ctx: &StatementContext,
    entry_index: i64,
    entry_details_index: i64,
    transaction_index: i64,
    batch: Option<&BatchInformation>,
    tx: &TxDtls,
    source: &str,
) -> Result<TransactionRow, String> {
    let (entry_amount, entry_currency) = money(ntry.amt.as_ref(), source)?;
    let (amount, currency) = money(tx.amt.as_ref(), source)?;
    let (batch_total_amount, batch_total_currency) =
        money(batch.and_then(|b| b.ttl_amt.as_ref()), source)?;
    let entry_code = ntry.bk_tx_cd.as_ref();
    let code = tx.bk_tx_cd.as_ref();
    let refs = tx.refs.as_ref();
    let parties = tx.rltd_pties.as_ref();
    let side =
        |pick: fn(&RltdPties) -> Option<&Party>| parties.and_then(pick).and_then(Party::name);

    Ok(TransactionRow {
        msg_id: ctx.msg_id.clone(),
        statement_kind: ctx.statement_kind.clone(),
        statement_index: Some(ctx.statement_index),
        statement_id: ctx.statement_id.clone(),
        account_iban: ctx.account_iban.clone(),
        account_currency: ctx.account_currency.clone(),
        entry_index: Some(entry_index),
        entry_ref: ntry.ntry_ref.clone(),
        entry_amount,
        entry_currency,
        entry_credit_debit: ntry.cdt_dbt_ind.clone(),
        entry_reversal_indicator: ntry.rvsl_ind.clone(),
        entry_status: ntry.sts.as_ref().and_then(|s| s.value()),
        booking_date: ntry.bookg_dt.as_ref().and_then(|d| d.value()),
        value_date: ntry.val_dt.as_ref().and_then(|d| d.value()),
        bank_ref: ntry.acct_svcr_ref.clone(),
        entry_bank_transaction_domain: entry_code.and_then(BankTransactionCode::domain),
        entry_bank_transaction_family: entry_code.and_then(BankTransactionCode::family),
        entry_bank_transaction_subfamily: entry_code.and_then(BankTransactionCode::subfamily),
        entry_bank_transaction_proprietary: entry_code.and_then(BankTransactionCode::proprietary),
        entry_bank_transaction_proprietary_issuer: entry_code
            .and_then(BankTransactionCode::proprietary_issuer),
        entry_details_index: Some(entry_details_index),
        transaction_index: Some(transaction_index),
        batch_message_id: batch.and_then(|b| b.msg_id.clone()),
        batch_payment_info_id: batch.and_then(|b| b.pmt_inf_id.clone()),
        batch_number_of_transactions: batch.and_then(|b| b.nb_of_txs.clone()),
        batch_total_amount,
        batch_total_currency,
        batch_credit_debit: batch.and_then(|b| b.cdt_dbt_ind.clone()),
        instruction_id: refs.and_then(|r| r.instr_id.clone()),
        end_to_end_id: refs.and_then(|r| r.end_to_end_id.clone()),
        transaction_id: refs.and_then(|r| r.tx_id.clone()),
        uetr: refs.and_then(|r| r.uetr.clone()),
        amount,
        currency,
        credit_debit: tx.cdt_dbt_ind.clone(),
        // Raw sides, each from its own element. An empty column here means the
        // message named nobody there, which is a fact about the message.
        debtor_name: side(|p| p.dbtr.as_ref()),
        debtor_account: account(parties.and_then(|p| p.dbtr_acct.as_ref())),
        ultimate_debtor_name: side(|p| p.ultmt_dbtr.as_ref()),
        creditor_name: side(|p| p.cdtr.as_ref()),
        creditor_account: account(parties.and_then(|p| p.cdtr_acct.as_ref())),
        ultimate_creditor_name: side(|p| p.ultmt_cdtr.as_ref()),
        bank_transaction_domain: code.and_then(BankTransactionCode::domain),
        bank_transaction_family: code.and_then(BankTransactionCode::family),
        bank_transaction_subfamily: code.and_then(BankTransactionCode::subfamily),
        bank_transaction_proprietary: code.and_then(BankTransactionCode::proprietary),
        bank_transaction_proprietary_issuer: code.and_then(BankTransactionCode::proprietary_issuer),
        remittance_count: Some(
            tx.rmt_inf
                .as_ref()
                .map_or(0, |rmt| rmt.leaves().count() as i64),
        ),
        source_file: Some(source.to_string()),
    })
}

/// The entry a cursor is walking, held once. One deserialized `<Ntry>`, its
/// statement context, and two integers - never a queue of the rows it will
/// produce, which would put an unbounded term into the memory bound.
struct OpenEntry {
    entry: Ntry,
    index: i64,
    ctx: StatementContext,
    cursor: TxCursor,
}

pub struct TransactionStream<R: BufRead> {
    records: StatementRecordStream<R, Ntry>,
    open: Option<OpenEntry>,
}

impl<R: BufRead> TransactionStream<R> {
    pub fn new(reader: R, source: &str) -> Self {
        TransactionStream {
            records: StatementRecordStream::new(reader, source),
            open: None,
        }
    }

    pub fn next_row(&mut self) -> Result<Option<TransactionRow>, Box<dyn Error>> {
        loop {
            if let Some(open) = self.open.as_mut() {
                if let Some((details_index, tx_index, tx)) = open.cursor.next(&open.entry) {
                    let batch = open
                        .entry
                        .ntry_dtls
                        .get(details_index as usize - 1)
                        .and_then(|details| details.btch.as_ref());
                    return Ok(Some(row_from_transaction(
                        &open.entry,
                        &open.ctx,
                        open.index,
                        details_index,
                        tx_index,
                        batch,
                        tx,
                        self.records.source(),
                    )?));
                }
                self.open = None;
            }
            let Some((index, entry)) = self.records.next_record()? else {
                return Ok(None);
            };
            self.open = Some(OpenEntry {
                entry,
                index,
                ctx: self.records.context().clone(),
                cursor: TxCursor::default(),
            });
        }
    }
}
