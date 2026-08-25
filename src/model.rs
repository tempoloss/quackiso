//! Serde model for the subset of camt.05x (bank-to-customer statement) that the
//! readers flatten into rows. Every field is optional: real-world messages omit
//! optional elements constantly, and a reader that panics on a missing tag is
//! useless. Missing -> None -> SQL NULL.
//!
//! Only the `<Ntry>` and `<Bal>` subtrees and their children are modelled. There
//! is no struct for the document or the statement, because nothing deserializes
//! one: the readers walk to each record as events and hand only that subtree to
//! serde.
//!
//! quick-xml's serde matches on local tag names, so the ISO 20022 default
//! namespace (`xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.xx"`) needs
//! no special handling here.
//!
//! The cursors at the bottom are how the four supplementary readers walk one
//! entry without materialising it. Each is one or two integers and a borrow of
//! the entry the stream already holds, so a batch of half a million
//! transactions costs what one of them costs - a `Vec` of rows built per entry
//! would add an unbounded term to `O(VECTOR_SIZE × row + largest subtree)`.

use crate::decimal;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Acct {
    #[serde(rename = "Id")]
    pub id: Option<AcctId>,
}

#[derive(Debug, Deserialize)]
pub struct AcctId {
    #[serde(rename = "IBAN")]
    pub iban: Option<String>,
    /// US and other non-IBAN accounts carry the number under Othr/Id.
    #[serde(rename = "Othr")]
    pub othr: Option<OtherId>,
}

#[derive(Debug, Deserialize)]
pub struct OtherId {
    #[serde(rename = "Id")]
    pub id: Option<String>,
}

impl AcctId {
    /// IBAN if present, else the "other" account identifier (US account no.).
    pub fn value(&self) -> Option<String> {
        self.iban
            .clone()
            .or_else(|| self.othr.as_ref().and_then(|o| o.id.clone()))
    }
}

#[derive(Debug, Deserialize)]
pub struct Ntry {
    #[serde(rename = "NtryRef")]
    pub ntry_ref: Option<String>,
    #[serde(rename = "Amt")]
    pub amt: Option<Amt>,
    #[serde(rename = "CdtDbtInd")]
    pub cdt_dbt_ind: Option<String>,
    /// `true` on an entry that takes a booking back. Reported as the wire spells
    /// it and never applied: an amount is unsigned here, and a reader that
    /// negated one would disagree with the balances beside it.
    #[serde(rename = "RvslInd")]
    pub rvsl_ind: Option<String>,
    #[serde(rename = "Sts")]
    pub sts: Option<CodeOrText>,
    #[serde(rename = "BookgDt")]
    pub bookg_dt: Option<DateChoice>,
    #[serde(rename = "ValDt")]
    pub val_dt: Option<DateChoice>,
    #[serde(rename = "AcctSvcrRef")]
    pub acct_svcr_ref: Option<String>,
    #[serde(rename = "BkTxCd")]
    pub bk_tx_cd: Option<BankTransactionCode>,
    #[serde(rename = "AmtDtls")]
    pub amt_dtls: Option<AmountDetails>,
    #[serde(rename = "NtryDtls", default)]
    pub ntry_dtls: Vec<NtryDtls>,
}

/// `<Amt Ccy="EUR">100.00</Amt>` — attribute + text content.
#[derive(Debug, Deserialize)]
pub struct Amt {
    #[serde(rename = "@Ccy")]
    pub ccy: Option<String>,
    #[serde(rename = "$text")]
    pub value: Option<String>,
}

/// An amount with its currency, exact, or an error naming the text that was
/// not a number.
///
/// Called from every camt row constructor, and only where the row it is
/// building actually carries the amount: an entry whose money is malformed and
/// whose transactions, balances and amount blocks are absent produces no row in
/// those functions, so nothing there has to read it. A NULL instead of an error
/// would disappear from a `SUM` and hand back a plausible wrong total.
pub fn money(amt: Option<&Amt>, source: &str) -> Result<(Option<i128>, Option<String>), String> {
    let value = decimal::scaled_opt(amt.and_then(|a| a.value.as_ref()))
        .map_err(|e| format!("{source}: {e}"))?;
    Ok((value, amt.and_then(|a| a.ccy.clone())))
}

/// Status appears as either `<Sts>BOOK</Sts>` (older) or `<Sts><Cd>BOOK</Cd></Sts>`
/// (2019+). One struct captures both: `Cd` child wins, else the text content.
#[derive(Debug, Deserialize)]
pub struct CodeOrText {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "$text")]
    pub text: Option<String>,
}

impl CodeOrText {
    pub fn value(&self) -> Option<String> {
        self.cd
            .clone()
            .or_else(|| self.text.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

/// `<BookgDt><Dt>2026-07-29</Dt></BookgDt>` or `<DtTm>...</DtTm>`.
#[derive(Debug, Deserialize)]
pub struct DateChoice {
    #[serde(rename = "Dt")]
    pub dt: Option<String>,
    #[serde(rename = "DtTm")]
    pub dt_tm: Option<String>,
}

impl DateChoice {
    pub fn value(&self) -> Option<String> {
        self.dt.clone().or_else(|| self.dt_tm.clone())
    }
}

#[derive(Debug, Deserialize)]
pub struct NtryDtls {
    /// The batch a group of transactions was posted as. Its counts and total
    /// describe the batch the bank booked, not this entry.
    #[serde(rename = "Btch")]
    pub btch: Option<BatchInformation>,
    #[serde(rename = "TxDtls", default)]
    pub tx_dtls: Vec<TxDtls>,
}

#[derive(Debug, Deserialize)]
pub struct TxDtls {
    #[serde(rename = "Refs")]
    pub refs: Option<Refs>,
    /// The transaction's own amount and direction. An entry of three payments
    /// states each one here; the entry's `Amt` is their sum, and neither stands
    /// in for the other.
    #[serde(rename = "Amt")]
    pub amt: Option<Amt>,
    #[serde(rename = "CdtDbtInd")]
    pub cdt_dbt_ind: Option<String>,
    #[serde(rename = "AmtDtls")]
    pub amt_dtls: Option<AmountDetails>,
    #[serde(rename = "BkTxCd")]
    pub bk_tx_cd: Option<BankTransactionCode>,
    #[serde(rename = "RltdPties")]
    pub rltd_pties: Option<RltdPties>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

#[derive(Debug, Deserialize)]
pub struct Refs {
    #[serde(rename = "InstrId")]
    pub instr_id: Option<String>,
    #[serde(rename = "EndToEndId")]
    pub end_to_end_id: Option<String>,
    #[serde(rename = "TxId")]
    pub tx_id: Option<String>,
    #[serde(rename = "UETR")]
    pub uetr: Option<String>,
}

/// `<Btch>`: which instruction batch these transactions were posted under, and
/// what the whole batch came to.
#[derive(Debug, Deserialize)]
pub struct BatchInformation {
    #[serde(rename = "MsgId")]
    pub msg_id: Option<String>,
    #[serde(rename = "PmtInfId")]
    pub pmt_inf_id: Option<String>,
    /// A wire count, kept as spelled. It is the sender's statement of how many
    /// transactions the batch held, which is not always how many are here.
    #[serde(rename = "NbOfTxs")]
    pub nb_of_txs: Option<String>,
    #[serde(rename = "TtlAmt")]
    pub ttl_amt: Option<Amt>,
    #[serde(rename = "CdtDbtInd")]
    pub cdt_dbt_ind: Option<String>,
}

/// `<BkTxCd>`: the bank's classification of what happened. Structured under
/// `Domn` or proprietary under `Prtry`, and a message may state either or both.
#[derive(Debug, Deserialize)]
pub struct BankTransactionCode {
    #[serde(rename = "Domn")]
    pub domn: Option<BankTransactionDomain>,
    #[serde(rename = "Prtry")]
    pub prtry: Option<ProprietaryBankTransactionCode>,
}

#[derive(Debug, Deserialize)]
pub struct BankTransactionDomain {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "Fmly")]
    pub fmly: Option<BankTransactionFamily>,
}

#[derive(Debug, Deserialize)]
pub struct BankTransactionFamily {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "SubFmlyCd")]
    pub sub_fmly_cd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProprietaryBankTransactionCode {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "Issr")]
    pub issr: Option<String>,
}

impl BankTransactionCode {
    /// The five leaves, in the order the columns declare them. Read straight
    /// off the wire: a proprietary code is not mapped onto a domain code and a
    /// missing domain is not filled in from the proprietary one, because the
    /// two are different vocabularies and a translation table nobody agreed on
    /// is worse than a NULL.
    pub fn domain(&self) -> Option<String> {
        self.domn.as_ref().and_then(|d| d.cd.clone())
    }

    pub fn family(&self) -> Option<String> {
        self.domn
            .as_ref()
            .and_then(|d| d.fmly.as_ref())
            .and_then(|f| f.cd.clone())
    }

    pub fn subfamily(&self) -> Option<String> {
        self.domn
            .as_ref()
            .and_then(|d| d.fmly.as_ref())
            .and_then(|f| f.sub_fmly_cd.clone())
    }

    pub fn proprietary(&self) -> Option<String> {
        self.prtry.as_ref().and_then(|p| p.cd.clone())
    }

    pub fn proprietary_issuer(&self) -> Option<String> {
        self.prtry.as_ref().and_then(|p| p.issr.clone())
    }
}

/// `<AmtDtls>`: the same money in the currencies and at the stages it passed
/// through. Four named slots and any number of proprietary ones.
#[derive(Debug, Deserialize)]
pub struct AmountDetails {
    #[serde(rename = "InstdAmt")]
    pub instd_amt: Option<AmountDetail>,
    #[serde(rename = "TxAmt")]
    pub tx_amt: Option<AmountDetail>,
    #[serde(rename = "CntrValAmt")]
    pub cntr_val_amt: Option<AmountDetail>,
    #[serde(rename = "AnncdPstngAmt")]
    pub anncd_pstng_amt: Option<AmountDetail>,
    #[serde(rename = "PrtryAmt", default)]
    pub prtry_amt: Vec<AmountDetail>,
}

/// One amount block. The four fixed slots and a proprietary one are the same
/// ISO type but for `Tp`, which only the proprietary one carries, so one struct
/// with an optional type serves both rather than two identical ones.
#[derive(Debug, Deserialize)]
pub struct AmountDetail {
    #[serde(rename = "Tp")]
    pub tp: Option<String>,
    #[serde(rename = "Amt")]
    pub amt: Option<Amt>,
    #[serde(rename = "CcyXchg")]
    pub ccy_xchg: Option<CurrencyExchange>,
}

#[derive(Debug, Deserialize)]
pub struct CurrencyExchange {
    #[serde(rename = "SrcCcy")]
    pub src_ccy: Option<String>,
    #[serde(rename = "TrgtCcy")]
    pub trgt_ccy: Option<String>,
    #[serde(rename = "UnitCcy")]
    pub unit_ccy: Option<String>,
    /// A rate, not money: kept as the lexical value the wire carried. Forcing
    /// it through the five fraction digits ISO 20022 allows an *amount* would
    /// either round a ten-digit rate or refuse the file over it.
    #[serde(rename = "XchgRate")]
    pub xchg_rate: Option<String>,
    #[serde(rename = "CtrctId")]
    pub ctrct_id: Option<String>,
    #[serde(rename = "QtnDt")]
    pub qtn_dt: Option<String>,
}

/// The kinds an amount block is reported as, in the order the schema states
/// them. `PROPRIETARY` is every repeated `<PrtryAmt>`, which carries its own
/// `Tp` beside it.
pub const AMOUNT_INSTRUCTED: &str = "INSTRUCTED";
pub const AMOUNT_TRANSACTION: &str = "TRANSACTION";
pub const AMOUNT_COUNTER_VALUE: &str = "COUNTER_VALUE";
pub const AMOUNT_ANNOUNCED_POSTING: &str = "ANNOUNCED_POSTING";
pub const AMOUNT_PROPRIETARY: &str = "PROPRIETARY";

impl AmountDetails {
    /// The `at`th block that is actually on the wire, zero-based, in schema
    /// order: the four fixed slots that are present, then the proprietary ones
    /// in document order.
    ///
    /// An absent fixed slot consumes no index, so a block that states only a
    /// `TxAmt` is index 1 and not index 2. Nothing is collected and nothing is
    /// re-walked: the four slots cost four `Option` checks whatever `at` is, and
    /// a proprietary block is reached by subtracting the fixed ones that are
    /// there from the index.
    pub fn block(&self, at: usize) -> Option<(&'static str, &AmountDetail)> {
        let fixed = [
            (AMOUNT_INSTRUCTED, self.instd_amt.as_ref()),
            (AMOUNT_TRANSACTION, self.tx_amt.as_ref()),
            (AMOUNT_COUNTER_VALUE, self.cntr_val_amt.as_ref()),
            (AMOUNT_ANNOUNCED_POSTING, self.anncd_pstng_amt.as_ref()),
        ];
        let present = fixed.iter().filter(|(_, block)| block.is_some()).count();
        if at >= present {
            return self
                .prtry_amt
                .get(at - present)
                .map(|block| (AMOUNT_PROPRIETARY, block));
        }
        fixed
            .into_iter()
            .filter_map(|(kind, block)| block.map(|block| (kind, block)))
            .nth(at)
    }
}

#[derive(Debug, Deserialize)]
pub struct RltdPties {
    #[serde(rename = "Dbtr")]
    pub dbtr: Option<Party>,
    #[serde(rename = "Cdtr")]
    pub cdtr: Option<Party>,
    #[serde(rename = "DbtrAcct")]
    pub dbtr_acct: Option<Acct>,
    #[serde(rename = "CdtrAcct")]
    pub cdtr_acct: Option<Acct>,
    /// Some statements name only the "ultimate" parties, with no immediate
    /// Dbtr/Cdtr at all (seen in genkgo's camt053.v2.minimal.ultimate).
    #[serde(rename = "UltmtDbtr")]
    pub ultmt_dbtr: Option<Party>,
    #[serde(rename = "UltmtCdtr")]
    pub ultmt_cdtr: Option<Party>,
}

#[derive(Debug, Deserialize)]
pub struct Party {
    /// camt.053.001.02: name sits directly under Dbtr/Cdtr.
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
    /// camt.053.001.08: name is nested one level deeper, under Pty.
    #[serde(rename = "Pty")]
    pub pty: Option<PartyInner>,
}

#[derive(Debug, Deserialize)]
pub struct PartyInner {
    #[serde(rename = "Nm")]
    pub nm: Option<String>,
}

impl Party {
    pub fn name(&self) -> Option<String> {
        self.nm
            .clone()
            .or_else(|| self.pty.as_ref().and_then(|p| p.nm.clone()))
    }
}

#[derive(Debug, Deserialize)]
pub struct RmtInf {
    #[serde(rename = "Ustrd", default)]
    pub ustrd: Vec<String>,
    /// Structured remittance. Many corporate statements carry no free-text
    /// Ustrd at all and put the invoice reference here instead.
    #[serde(rename = "Strd", default)]
    pub strd: Vec<Strd>,
}

#[derive(Debug, Deserialize)]
pub struct Strd {
    #[serde(rename = "CdtrRefInf")]
    pub cdtr_ref_inf: Option<CdtrRefInf>,
    #[serde(rename = "AddtlRmtInf", default)]
    pub addtl: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CdtrRefInf {
    #[serde(rename = "Ref")]
    pub reference: Option<String>,
}

/// The slots a remittance leaf can sit in.
pub const REMITTANCE_UNSTRUCTURED: &str = "UNSTRUCTURED";
pub const REMITTANCE_CREDITOR_REFERENCE: &str = "CREDITOR_REFERENCE";
pub const REMITTANCE_ADDITIONAL: &str = "ADDITIONAL";

/// One remittance text leaf, with where it sat.
#[derive(Debug, Clone, Copy)]
pub struct RemittanceLeaf<'a> {
    pub slot: &'static str,
    /// The 1-based document ordinal of the owning `<Strd>`, or None for a
    /// `<Ustrd>`. Earlier `<Strd>` blocks that carried no supported leaf are
    /// still counted, so this is the block's position in the message and not a
    /// position in the output.
    pub structured_index: Option<i64>,
    pub text: &'a str,
}

impl RmtInf {
    /// Every supported non-empty text leaf, in slot order: the `<Ustrd>`
    /// occurrences in document order, then each `<Strd>` in document order with
    /// its creditor reference ahead of its additional text.
    ///
    /// Blank leaves are skipped rather than emitted: `<Ustrd/>` is an element on
    /// the wire and not a remittance. Nothing is joined and nothing is dropped
    /// for being a duplicate - two invoices in two `<Ustrd>` are two facts, and
    /// the string `"A B"` cannot be taken apart again.
    pub fn leaves(&self) -> impl Iterator<Item = RemittanceLeaf<'_>> {
        let mut slot = LeafSlot::default();
        std::iter::from_fn(move || slot.next(self))
    }
}

/// How many supported remittance leaves a transaction states, and the text when
/// there is exactly one of them.
///
/// This replaced a helper that returned the first slot that had anything in it,
/// joined by spaces. That answered "what does this payment say" with one of the
/// several things it said, and a caller had no way to know which: a transaction
/// carrying both free text and a structured reference reported only the free
/// text, and two invoice numbers came back as one string.
fn remittance(tx: &TxDtls) -> (i64, Option<String>) {
    let Some(rmt) = tx.rmt_inf.as_ref() else {
        return (0, None);
    };
    let mut count = 0i64;
    let mut sole: Option<String> = None;
    for leaf in rmt.leaves() {
        count += 1;
        sole = match count {
            1 => Some(leaf.text.to_string()),
            _ => None,
        };
    }
    (count, sole)
}

/// One flattened output row: a single booked entry (`Ntry`) with its statement
/// context resolved, the counts that say what is under it, and the
/// transaction-derived columns only where one transaction answers for the
/// entry. This is the grain of `read_iso20022`.
#[derive(Debug, Default, Clone)]
pub struct Row {
    pub msg_id: Option<String>,
    pub account_iban: Option<String>,
    pub statement_id: Option<String>,
    pub entry_ref: Option<String>,
    /// Exact amount scaled by `10^decimal::SCALE`; never a float.
    pub amount: Option<i128>,
    pub currency: Option<String>,
    pub credit_debit: Option<String>,
    pub status: Option<String>,
    pub booking_date: Option<String>,
    pub value_date: Option<String>,
    pub bank_ref: Option<String>,
    /// The four convenience columns, populated only where they are not a guess:
    /// the first three when the entry has exactly one transaction, the fourth
    /// when that transaction also has exactly one remittance leaf.
    pub end_to_end_id: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty_iban: Option<String>,
    pub remittance_info: Option<String>,
    /// `Stmt`, `Ntfctn` or `Rpt`, and the two indexes the supplementary readers
    /// join on. All three are NULL for an `<Ntry>` that is not a direct child of
    /// a statement: ADR 0004 keeps such an entry as a row, and a scoped index
    /// for it would be a join key pointing at nothing.
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub entry_index: Option<i64>,
    /// Exact counts, zero included: every entry has a number of transactions
    /// and a number of remittance leaves, so these are facts rather than
    /// absences.
    pub transaction_count: i64,
    pub remittance_count: i64,
    pub reversal_indicator: Option<String>,
    pub bank_transaction_domain: Option<String>,
    pub bank_transaction_family: Option<String>,
    pub bank_transaction_subfamily: Option<String>,
    pub bank_transaction_proprietary: Option<String>,
    pub bank_transaction_proprietary_issuer: Option<String>,
    pub source_file: Option<String>,
}

/// What the statement around an entry says about it. Passed as one value
/// because the reader carries all six together and a six-argument constructor
/// is where an account number ends up in the statement id.
#[derive(Debug, Default, Clone)]
pub struct EntryCtx {
    pub msg_id: Option<String>,
    pub account_iban: Option<String>,
    pub statement_id: Option<String>,
    /// The three scope fields, set only for a direct child of the active
    /// statement.
    pub statement_kind: Option<String>,
    pub statement_index: Option<i64>,
    pub entry_index: Option<i64>,
}

/// Build one output row from a single entry plus its statement context.
///
/// One nested pass over `NtryDtls/TxDtls` produces the counts and, when there
/// is exactly one transaction, that transaction. Nothing is flattened and
/// nothing is rescanned per column: the alternative was four walks of the same
/// nesting and a first-transaction answer standing in for all of them.
///
/// Fails rather than nulling a malformed amount: a NULL would disappear from a
/// `SUM` and hand back a plausible wrong total.
pub fn row_from_entry(ntry: &Ntry, ctx: &EntryCtx, source: &str) -> Result<Row, String> {
    let mut transaction_count = 0i64;
    let mut remittance_count = 0i64;
    let mut sole: Option<&TxDtls> = None;
    let mut sole_remittance: Option<String> = None;
    for details in &ntry.ntry_dtls {
        for tx in &details.tx_dtls {
            transaction_count += 1;
            let (count, text) = remittance(tx);
            remittance_count += count;
            if transaction_count == 1 {
                sole = Some(tx);
                sole_remittance = text;
            }
        }
    }
    // One transaction, and it is the entry. Two or more, and any of these
    // columns would be one payment's answer to a question about three.
    let (sole, sole_remittance) = match transaction_count {
        1 => (sole, sole_remittance),
        _ => (None, None),
    };
    // Direction: the transaction's own when it stated one, else the entry's.
    // The counterparty is the party on the other side of *that* flow.
    let direction = sole
        .and_then(|tx| tx.cdt_dbt_ind.as_deref())
        .or(ntry.cdt_dbt_ind.as_deref());
    let (cp_name, cp_iban) = counterparty(direction, sole);
    let code = ntry.bk_tx_cd.as_ref();

    Ok(Row {
        msg_id: ctx.msg_id.clone(),
        account_iban: ctx.account_iban.clone(),
        statement_id: ctx.statement_id.clone(),
        entry_ref: ntry.ntry_ref.clone(),
        amount: decimal::scaled_opt(ntry.amt.as_ref().and_then(|a| a.value.as_ref()))
            .map_err(|e| format!("{source}: {e}"))?,
        currency: ntry.amt.as_ref().and_then(|a| a.ccy.clone()),
        credit_debit: ntry.cdt_dbt_ind.clone(),
        status: ntry.sts.as_ref().and_then(|s| s.value()),
        booking_date: ntry.bookg_dt.as_ref().and_then(|d| d.value()),
        value_date: ntry.val_dt.as_ref().and_then(|d| d.value()),
        bank_ref: ntry.acct_svcr_ref.clone(),
        end_to_end_id: sole
            .and_then(|t| t.refs.as_ref())
            .and_then(|r| r.end_to_end_id.clone()),
        counterparty_name: cp_name,
        counterparty_iban: cp_iban,
        // One leaf, or nothing. A sole transaction carrying free text *and* a
        // structured reference has two answers here and no way to say which,
        // so it says neither and `read_camt_remittance` has both.
        remittance_info: match remittance_count {
            1 => sole_remittance,
            _ => None,
        },
        statement_kind: ctx.statement_kind.clone(),
        statement_index: ctx.statement_index,
        entry_index: ctx.entry_index,
        transaction_count,
        remittance_count,
        reversal_indicator: ntry.rvsl_ind.clone(),
        bank_transaction_domain: code.and_then(BankTransactionCode::domain),
        bank_transaction_family: code.and_then(BankTransactionCode::family),
        bank_transaction_subfamily: code.and_then(BankTransactionCode::subfamily),
        bank_transaction_proprietary: code.and_then(BankTransactionCode::proprietary),
        bank_transaction_proprietary_issuer: code.and_then(BankTransactionCode::proprietary_issuer),
        source_file: Some(source.to_string()),
    })
}

/// Resolve the counterparty: the party on the *other* side of the flow.
///
/// Real statements routinely populate only one side — a CRDT entry may carry
/// just `<Cdtr>` — so the other side answers when the correct one says nothing
/// at all. Name and account always come from the same side: one party's name
/// beside another party's account describes nobody. `UltmtDbtr`/`UltmtCdtr`
/// belong to their own side and stand in when it names no immediate party.
fn counterparty(cdt_dbt: Option<&str>, tx: Option<&TxDtls>) -> (Option<String>, Option<String>) {
    let Some(rp) = tx.and_then(|t| t.rltd_pties.as_ref()) else {
        return (None, None);
    };
    // money out (DBIT) -> the creditor is who we paid; money in -> the debtor
    let (first, second) = match cdt_dbt {
        Some("CRDT") => (rp.dbtr.as_ref(), rp.cdtr.as_ref()),
        _ => (rp.cdtr.as_ref(), rp.dbtr.as_ref()),
    };
    let (first_acct, second_acct) = match cdt_dbt {
        Some("CRDT") => (rp.dbtr_acct.as_ref(), rp.cdtr_acct.as_ref()),
        _ => (rp.cdtr_acct.as_ref(), rp.dbtr_acct.as_ref()),
    };
    let (ultmt_first, ultmt_second) = match cdt_dbt {
        Some("CRDT") => (rp.ultmt_dbtr.as_ref(), rp.ultmt_cdtr.as_ref()),
        _ => (rp.ultmt_cdtr.as_ref(), rp.ultmt_dbtr.as_ref()),
    };

    let acct_value = |a: Option<&Acct>| a.and_then(|a| a.id.as_ref()).and_then(|i| i.value());

    // One side, both fields. A name from the correct side beside an account
    // from the other describes two parties in one row. The ultimate party is
    // that side's fallback name, not a fourth candidate: `RltdPties` has no
    // account element for it, so pairing it with a foreign account would be
    // the very mix this loop exists to prevent.
    for (party, ultmt, acct) in [
        (first, ultmt_first, first_acct),
        (second, ultmt_second, second_acct),
    ] {
        let name = party
            .and_then(|p| p.name())
            .or_else(|| ultmt.and_then(|p| p.name()));
        let iban = acct_value(acct);
        if name.is_some() || iban.is_some() {
            return (name, iban);
        }
    }
    (None, None)
}

// ── balances ─────────────────────────────────────────────────────────────────

/// `<Bal>`: an account's position at a moment, which is the other half of what
/// a statement says. A statement of balances and no movements is a complete
/// statement, and until there was a grain for this the reader truthfully
/// returned nothing for one.
#[derive(Debug, Deserialize)]
pub struct Balance {
    #[serde(rename = "Tp")]
    pub tp: Option<BalanceType>,
    #[serde(rename = "Amt")]
    pub amt: Option<Amt>,
    #[serde(rename = "CdtDbtInd")]
    pub cdt_dbt_ind: Option<String>,
    #[serde(rename = "Dt")]
    pub dt: Option<DateChoice>,
}

#[derive(Debug, Deserialize)]
pub struct BalanceType {
    #[serde(rename = "CdOrPrtry")]
    pub cd_or_prtry: Option<CodeOrProprietary>,
    #[serde(rename = "SubTp")]
    pub sub_tp: Option<CodeOrProprietary>,
}

/// A code from the published list, or the bank's own name for something the
/// list does not have. Which of the two it was is reported beside the value:
/// `OPBD` and a proprietary `INTRADAY-PEAK` are not the same kind of fact, and
/// a single column could not say so.
#[derive(Debug, Deserialize)]
pub struct CodeOrProprietary {
    #[serde(rename = "Cd")]
    pub cd: Option<String>,
    #[serde(rename = "Prtry")]
    pub prtry: Option<String>,
}

pub const SCHEME_CODE: &str = "CODE";
pub const SCHEME_PROPRIETARY: &str = "PROPRIETARY";

impl CodeOrProprietary {
    /// The value and which scheme it came from, or None when the element is
    /// present and empty.
    pub fn value(&self) -> Option<(String, &'static str)> {
        if let Some(code) = self.cd.as_ref().filter(|c| !c.trim().is_empty()) {
            return Some((code.clone(), SCHEME_CODE));
        }
        self.prtry
            .as_ref()
            .filter(|p| !p.trim().is_empty())
            .map(|p| (p.clone(), SCHEME_PROPRIETARY))
    }
}

impl Balance {
    pub fn kind(&self) -> Option<(String, &'static str)> {
        self.tp
            .as_ref()
            .and_then(|t| t.cd_or_prtry.as_ref())
            .and_then(CodeOrProprietary::value)
    }

    pub fn subkind(&self) -> Option<(String, &'static str)> {
        self.tp
            .as_ref()
            .and_then(|t| t.sub_tp.as_ref())
            .and_then(CodeOrProprietary::value)
    }
}

// ── cursors over one entry ───────────────────────────────────────────────────

/// Where a scan of an entry's transactions is: which `<NtryDtls>` and which
/// `<TxDtls>` inside it. Two integers, advanced in place, so a stream holds one
/// deserialized entry and this - never a queue of the rows it will produce.
#[derive(Debug, Default, Clone, Copy)]
pub struct TxCursor {
    details: usize,
    tx: usize,
}

impl TxCursor {
    /// The next transaction, with the 1-based indexes of its details block and
    /// of itself inside that block. Both are scoped, not running totals: two
    /// transactions under one `<NtryDtls>` are 1 and 2, and the first
    /// transaction of the next block is 1 again.
    pub fn next<'a>(&mut self, ntry: &'a Ntry) -> Option<(i64, i64, &'a TxDtls)> {
        while let Some(details) = ntry.ntry_dtls.get(self.details) {
            if let Some(tx) = details.tx_dtls.get(self.tx) {
                let at = (self.details as i64 + 1, self.tx as i64 + 1);
                self.tx += 1;
                return Some((at.0, at.1, tx));
            }
            self.details += 1;
            self.tx = 0;
        }
        None
    }
}

/// Where a scan of an entry's amount blocks is: the entry's own `<AmtDtls>`
/// first, then each transaction's.
///
/// The boundary between the two scopes is the one thing here worth a test of
/// its own: an entry with no `<AmtDtls>` of its own must not skip the
/// transactions' blocks, and one with six of them must not report the seventh
/// as entry-level.
#[derive(Debug, Default, Clone, Copy)]
pub struct AmountCursor {
    /// None while the entry's own blocks are being walked.
    tx: Option<TxCursor>,
    /// The transaction being walked, once the entry's blocks are done.
    at: Option<(i64, i64)>,
    block: usize,
}

/// One amount block with where it sat.
pub struct AmountSite<'a> {
    /// `ENTRY` or `TRANSACTION`.
    pub scope: &'static str,
    /// NULL for an entry-level block; the transaction's scoped indexes
    /// otherwise.
    pub entry_details_index: Option<i64>,
    pub transaction_index: Option<i64>,
    pub kind: &'static str,
    /// 1-based among the blocks emitted from the owning `<AmtDtls>`.
    pub index: i64,
    pub detail: &'a AmountDetail,
}

pub const SCOPE_ENTRY: &str = "ENTRY";
pub const SCOPE_TRANSACTION: &str = "TRANSACTION";

impl AmountCursor {
    pub fn next<'a>(&mut self, ntry: &'a Ntry) -> Option<AmountSite<'a>> {
        // the entry's own blocks
        if self.tx.is_none() {
            if let Some((kind, detail)) = ntry
                .amt_dtls
                .as_ref()
                .and_then(|details| details.block(self.block))
            {
                self.block += 1;
                return Some(AmountSite {
                    scope: SCOPE_ENTRY,
                    entry_details_index: None,
                    transaction_index: None,
                    kind,
                    index: self.block as i64,
                    detail,
                });
            }
            self.tx = Some(TxCursor::default());
            self.block = 0;
        }
        // then each transaction's, in the order the transactions come
        loop {
            let cursor = self.tx.as_mut().expect("set just above");
            if let Some((details_index, tx_index)) = self.at {
                let tx = transaction_at(ntry, details_index, tx_index);
                if let Some((kind, detail)) = tx
                    .and_then(|tx| tx.amt_dtls.as_ref())
                    .and_then(|details| details.block(self.block))
                {
                    self.block += 1;
                    return Some(AmountSite {
                        scope: SCOPE_TRANSACTION,
                        entry_details_index: Some(details_index),
                        transaction_index: Some(tx_index),
                        kind,
                        index: self.block as i64,
                        detail,
                    });
                }
            }
            let (details_index, tx_index, _) = cursor.next(ntry)?;
            self.at = Some((details_index, tx_index));
            self.block = 0;
        }
    }
}

/// Where a scan of an entry's remittance leaves is: which transaction, and
/// which leaf of it.
#[derive(Debug, Default, Clone, Copy)]
pub struct RemittanceCursor {
    tx: TxCursor,
    at: Option<(i64, i64)>,
    /// Where in the transaction's slots the next leaf comes from. Stepped, not
    /// replayed: an ordinal handed back to `leaves().nth()` re-walked every
    /// earlier leaf for every row, which is quadratic in a transaction that
    /// states hundreds of `<Ustrd>` - and they do.
    slot: LeafSlot,
    emitted: i64,
}

/// The next leaf position inside one transaction's `RmtInf`, and the one
/// definition of what leaf order is. `RmtInf::leaves` steps the same state, so
/// the count a legacy entry column reports and the index a remittance row
/// carries cannot drift apart.
#[derive(Debug, Clone, Copy)]
enum LeafSlot {
    /// The next `<Ustrd>` to look at.
    Unstructured { at: usize },
    /// Inside `<Strd>` number `at`: its creditor reference first, then its
    /// additional texts from `addtl` on.
    Structured {
        at: usize,
        reference: bool,
        addtl: usize,
    },
}

impl Default for LeafSlot {
    fn default() -> Self {
        LeafSlot::Unstructured { at: 0 }
    }
}

impl LeafSlot {
    /// The next supported non-empty leaf at or after this position, advancing
    /// past every slot it looked at. Blank leaves are stepped over rather than
    /// emitted: `<Ustrd/>` is an element on the wire and not a remittance.
    fn next<'a>(&mut self, rmt: &'a RmtInf) -> Option<RemittanceLeaf<'a>> {
        loop {
            match *self {
                LeafSlot::Unstructured { at } => match rmt.ustrd.get(at) {
                    None => {
                        *self = LeafSlot::Structured {
                            at: 0,
                            reference: true,
                            addtl: 0,
                        }
                    }
                    Some(text) => {
                        *self = LeafSlot::Unstructured { at: at + 1 };
                        if !text.trim().is_empty() {
                            return Some(RemittanceLeaf {
                                slot: REMITTANCE_UNSTRUCTURED,
                                structured_index: None,
                                text,
                            });
                        }
                    }
                },
                LeafSlot::Structured {
                    at,
                    reference,
                    addtl,
                } => {
                    // Past the last block, so the transaction is done. An
                    // earlier block that carried nothing still spent its
                    // ordinal, which is what `structured_index` reports.
                    let strd = rmt.strd.get(at)?;
                    let ordinal = at as i64 + 1;
                    if reference {
                        *self = LeafSlot::Structured {
                            at,
                            reference: false,
                            addtl: 0,
                        };
                        let text = strd
                            .cdtr_ref_inf
                            .as_ref()
                            .and_then(|c| c.reference.as_deref())
                            .filter(|text| !text.trim().is_empty());
                        if let Some(text) = text {
                            return Some(RemittanceLeaf {
                                slot: REMITTANCE_CREDITOR_REFERENCE,
                                structured_index: Some(ordinal),
                                text,
                            });
                        }
                        continue;
                    }
                    match strd.addtl.get(addtl) {
                        None => {
                            *self = LeafSlot::Structured {
                                at: at + 1,
                                reference: true,
                                addtl: 0,
                            }
                        }
                        Some(text) => {
                            *self = LeafSlot::Structured {
                                at,
                                reference: false,
                                addtl: addtl + 1,
                            };
                            if !text.trim().is_empty() {
                                return Some(RemittanceLeaf {
                                    slot: REMITTANCE_ADDITIONAL,
                                    structured_index: Some(ordinal),
                                    text,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One remittance leaf with where it sat.
pub struct RemittanceSite<'a> {
    pub entry_details_index: i64,
    pub transaction_index: i64,
    /// 1-based among the leaves of this transaction, in slot order.
    pub index: i64,
    pub leaf: RemittanceLeaf<'a>,
}

impl RemittanceCursor {
    pub fn next<'a>(&mut self, ntry: &'a Ntry) -> Option<RemittanceSite<'a>> {
        loop {
            if let Some((details_index, tx_index)) = self.at {
                let leaf = transaction_at(ntry, details_index, tx_index)
                    .and_then(|tx| tx.rmt_inf.as_ref())
                    .and_then(|rmt| self.slot.next(rmt));
                if let Some(leaf) = leaf {
                    self.emitted += 1;
                    return Some(RemittanceSite {
                        entry_details_index: details_index,
                        transaction_index: tx_index,
                        index: self.emitted,
                        leaf,
                    });
                }
            }
            let (details_index, tx_index, _) = self.tx.next(ntry)?;
            self.at = Some((details_index, tx_index));
            self.slot = LeafSlot::default();
            self.emitted = 0;
        }
    }
}

/// The transaction at a pair of 1-based scoped indexes. The cursors hand out
/// the indexes and come back for the transaction, so the borrow of the entry
/// ends between calls and the cursor stays two integers.
fn transaction_at(ntry: &Ntry, details_index: i64, tx_index: i64) -> Option<&TxDtls> {
    ntry.ntry_dtls
        .get(details_index as usize - 1)?
        .tx_dtls
        .get(tx_index as usize - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `<Ntry>` subtree, through the same deserializer the readers use, so
    /// a misspelled `#[serde(rename)]` fails here rather than reading NULL.
    fn entry(body: &str) -> Ntry {
        quick_xml::de::from_str(&format!("<Ntry>{body}</Ntry>")).expect("the entry parses")
    }

    fn row(body: &str) -> Row {
        row_from_entry(&entry(body), &EntryCtx::default(), "test.xml").expect("the entry projects")
    }

    /// One transaction under one details block, with everything the
    /// convenience columns read.
    const ONE_TX: &str = "<NtryDtls><TxDtls><Refs><EndToEndId>E2E-1</EndToEndId></Refs>\
                          <RltdPties><Cdtr><Nm>Rheintal GmbH</Nm></Cdtr>\
                          <CdtrAcct><Id><IBAN>CH5604835012345678009</IBAN></Id></CdtrAcct>\
                          </RltdPties><RmtInf><Ustrd>Invoice 1</Ustrd></RmtInf>\
                          </TxDtls></NtryDtls>";

    #[test]
    fn model_one_transaction_answers_for_the_entry() {
        let got = row(&format!(
            "<Amt Ccy=\"CHF\">100.00</Amt><CdtDbtInd>DBIT</CdtDbtInd>{ONE_TX}"
        ));
        assert_eq!(got.transaction_count, 1);
        assert_eq!(got.remittance_count, 1);
        assert_eq!(got.end_to_end_id.as_deref(), Some("E2E-1"));
        assert_eq!(got.counterparty_name.as_deref(), Some("Rheintal GmbH"));
        assert_eq!(
            got.counterparty_iban.as_deref(),
            Some("CH5604835012345678009")
        );
        assert_eq!(got.remittance_info.as_deref(), Some("Invoice 1"));
    }

    /// The defect this slice is about. Three transactions under three details
    /// blocks used to report the first one's end-to-end id, counterparty and
    /// remittance for the whole entry.
    #[test]
    fn model_three_transactions_answer_for_none_of_them() {
        let mut body = String::from("<Amt Ccy=\"CHF\">900.00</Amt><CdtDbtInd>DBIT</CdtDbtInd>");
        for at in 1..=3 {
            body.push_str(&format!(
                "<NtryDtls><TxDtls><Refs><EndToEndId>E2E-{at}</EndToEndId></Refs>\
                 <RltdPties><Cdtr><Nm>Party {at}</Nm></Cdtr></RltdPties>\
                 <RmtInf><Ustrd>Invoice {at}</Ustrd></RmtInf></TxDtls></NtryDtls>"
            ));
        }
        let got = row(&body);
        assert_eq!(got.transaction_count, 3);
        assert_eq!(got.remittance_count, 3);
        assert_eq!(got.end_to_end_id, None);
        assert_eq!(got.counterparty_name, None);
        assert_eq!(got.counterparty_iban, None);
        assert_eq!(got.remittance_info, None);
    }

    /// Repeated `<TxDtls>` inside one `<NtryDtls>`: the count is transactions
    /// and not details blocks, which a walk of `ntry_dtls.len()` would get
    /// wrong on exactly this shape.
    #[test]
    fn model_two_transactions_in_one_details_block_are_two() {
        let got = row("<Amt Ccy=\"CHF\">50.00</Amt><NtryDtls>\
             <TxDtls><Refs><EndToEndId>A</EndToEndId></Refs></TxDtls>\
             <TxDtls><Refs><EndToEndId>B</EndToEndId></Refs></TxDtls></NtryDtls>");
        assert_eq!(got.transaction_count, 2);
        assert_eq!(got.end_to_end_id, None);

        let mut cursor = TxCursor::default();
        let e = entry(
            "<NtryDtls><TxDtls><Refs><EndToEndId>A</EndToEndId></Refs></TxDtls>\
             <TxDtls><Refs><EndToEndId>B</EndToEndId></Refs></TxDtls></NtryDtls>",
        );
        let mut seen = Vec::new();
        while let Some((details, tx, value)) = cursor.next(&e) {
            seen.push((
                details,
                tx,
                value
                    .refs
                    .as_ref()
                    .and_then(|r| r.end_to_end_id.clone())
                    .unwrap_or_default(),
            ));
        }
        assert_eq!(
            seen,
            [(1, 1, "A".to_string()), (1, 2, "B".to_string())],
            "the transaction index is scoped to its details block"
        );
    }

    #[test]
    fn model_an_entry_with_no_transactions_counts_zero_and_claims_nothing() {
        let got = row("<Amt Ccy=\"CHF\">10.00</Amt><CdtDbtInd>CRDT</CdtDbtInd>");
        assert_eq!(got.transaction_count, 0);
        assert_eq!(got.remittance_count, 0);
        assert_eq!(got.end_to_end_id, None);
        assert_eq!(got.remittance_info, None);
        assert_eq!(got.amount, Some(1_000_000));
    }

    /// The transaction's own direction decides the counterparty when it states
    /// one. An entry booked DBIT holding a CRDT transaction is a real shape -
    /// a reversal inside a batch - and reading the entry's direction there
    /// names the party on the wrong end of it.
    #[test]
    fn model_the_transactions_own_direction_decides_the_counterparty() {
        let parties = "<RltdPties><Dbtr><Nm>Payer</Nm></Dbtr><Cdtr><Nm>Payee</Nm></Cdtr>\
                       </RltdPties>";
        let dbit = row(&format!(
            "<CdtDbtInd>DBIT</CdtDbtInd><NtryDtls><TxDtls>{parties}</TxDtls></NtryDtls>"
        ));
        assert_eq!(dbit.counterparty_name.as_deref(), Some("Payee"));
        let crdt = row(&format!(
            "<CdtDbtInd>DBIT</CdtDbtInd><NtryDtls><TxDtls><CdtDbtInd>CRDT</CdtDbtInd>\
             {parties}</TxDtls></NtryDtls>"
        ));
        assert_eq!(
            crdt.counterparty_name.as_deref(),
            Some("Payer"),
            "the transaction said CRDT, so the debtor is the counterparty"
        );
        assert_eq!(
            crdt.credit_debit.as_deref(),
            Some("DBIT"),
            "the entry's own direction column is still the entry's"
        );
    }

    /// Both bank-code vocabularies, read as themselves. A proprietary code is
    /// not mapped onto a domain code and a missing domain is not filled in from
    /// the proprietary one.
    #[test]
    fn model_bank_transaction_codes_are_read_without_fallback() {
        let structured = row("<BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>ICDT</Cd>\
             <SubFmlyCd>ESCT</SubFmlyCd></Fmly></Domn></BkTxCd>");
        assert_eq!(structured.bank_transaction_domain.as_deref(), Some("PMNT"));
        assert_eq!(structured.bank_transaction_family.as_deref(), Some("ICDT"));
        assert_eq!(
            structured.bank_transaction_subfamily.as_deref(),
            Some("ESCT")
        );
        assert_eq!(structured.bank_transaction_proprietary, None);
        assert_eq!(structured.bank_transaction_proprietary_issuer, None);

        let proprietary = row("<BkTxCd><Prtry><Cd>NRTF</Cd><Issr>SIX</Issr></Prtry></BkTxCd>");
        assert_eq!(proprietary.bank_transaction_domain, None);
        assert_eq!(proprietary.bank_transaction_family, None);
        assert_eq!(proprietary.bank_transaction_subfamily, None);
        assert_eq!(
            proprietary.bank_transaction_proprietary.as_deref(),
            Some("NRTF")
        );
        assert_eq!(
            proprietary.bank_transaction_proprietary_issuer.as_deref(),
            Some("SIX")
        );
    }

    #[test]
    fn model_the_reversal_indicator_is_reported_and_never_applied() {
        let got =
            row("<Amt Ccy=\"CHF\">170.00</Amt><CdtDbtInd>CRDT</CdtDbtInd><RvslInd>true</RvslInd>");
        assert_eq!(got.reversal_indicator.as_deref(), Some("true"));
        assert_eq!(got.amount, Some(17_000_000), "unsigned, as the wire said");
        assert_eq!(got.credit_debit.as_deref(), Some("CRDT"));
    }

    // ── remittance ───────────────────────────────────────────────────────────

    fn leaves(rmt: &str) -> Vec<(&'static str, Option<i64>, String)> {
        let parsed: RmtInf = quick_xml::de::from_str(rmt).expect("the block parses");
        parsed
            .leaves()
            .map(|leaf| (leaf.slot, leaf.structured_index, leaf.text.to_string()))
            .collect()
    }

    /// Slot order, and every occurrence kept. Two invoices in two `<Ustrd>` are
    /// two facts; the string `"Invoice 1 Invoice 2"` cannot be taken apart.
    #[test]
    fn model_remittance_leaves_come_out_in_slot_order() {
        assert_eq!(
            leaves(
                "<RmtInf><Ustrd>Invoice 1</Ustrd><Ustrd>Invoice 2</Ustrd>\
                 <Strd><CdtrRefInf><Ref>RF18</Ref></CdtrRefInf></Strd>\
                 <Strd><AddtlRmtInf>Tranche 1</AddtlRmtInf></Strd></RmtInf>"
            ),
            [
                ("UNSTRUCTURED", None, "Invoice 1".to_string()),
                ("UNSTRUCTURED", None, "Invoice 2".to_string()),
                ("CREDITOR_REFERENCE", Some(1), "RF18".to_string()),
                ("ADDITIONAL", Some(2), "Tranche 1".to_string()),
            ]
        );
    }

    /// Inside one `<Strd>` the creditor reference comes before the additional
    /// text, and a block that emitted nothing still counts towards the ordinal
    /// of the ones after it: `structured_index` is a position in the message.
    #[test]
    fn model_a_structured_block_that_says_nothing_still_takes_its_ordinal() {
        assert_eq!(
            leaves(
                "<RmtInf><Strd><RfrdDocInf><Nb>INV-1</Nb></RfrdDocInf></Strd>\
                 <Strd><AddtlRmtInf>Second</AddtlRmtInf>\
                 <CdtrRefInf><Ref>RF92</Ref></CdtrRefInf></Strd></RmtInf>"
            ),
            [
                ("CREDITOR_REFERENCE", Some(2), "RF92".to_string()),
                ("ADDITIONAL", Some(2), "Second".to_string()),
            ]
        );
    }

    /// A blank leaf is an element on the wire and not a remittance. Both
    /// spellings: self-closing, and open-and-closed around whitespace.
    #[test]
    fn model_a_blank_remittance_leaf_is_omitted() {
        assert_eq!(
            leaves(
                "<RmtInf><Ustrd/><Ustrd>   </Ustrd><Ustrd>Invoice 9</Ustrd>\
                 <Strd><CdtrRefInf><Ref></Ref></CdtrRefInf>\
                 <AddtlRmtInf> </AddtlRmtInf></Strd></RmtInf>"
            ),
            [("UNSTRUCTURED", None, "Invoice 9".to_string())]
        );
    }

    /// Two leaves in two slots on a sole transaction. The entry row has one
    /// column for the answer and two answers to put in it, so it has neither.
    #[test]
    fn model_two_leaves_leave_the_entry_column_empty() {
        let got = row("<NtryDtls><TxDtls><RmtInf><Ustrd>Refund</Ustrd>\
             <Strd><CdtrRefInf><Ref>RF92</Ref></CdtrRefInf></Strd>\
             </RmtInf></TxDtls></NtryDtls>");
        assert_eq!(got.transaction_count, 1);
        assert_eq!(got.remittance_count, 2);
        assert_eq!(got.remittance_info, None);
    }

    #[test]
    fn model_the_remittance_cursor_numbers_leaves_inside_each_transaction() {
        let e = entry(
            "<NtryDtls><TxDtls><RmtInf><Ustrd>A</Ustrd><Ustrd>B</Ustrd></RmtInf></TxDtls>\
             <TxDtls><RmtInf><Strd><AddtlRmtInf>C</AddtlRmtInf></Strd></RmtInf></TxDtls>\
             </NtryDtls><NtryDtls><TxDtls></TxDtls>\
             <TxDtls><RmtInf><Ustrd>D</Ustrd></RmtInf></TxDtls></NtryDtls>",
        );
        let mut cursor = RemittanceCursor::default();
        let mut seen = Vec::new();
        while let Some(site) = cursor.next(&e) {
            seen.push((
                site.entry_details_index,
                site.transaction_index,
                site.index,
                site.leaf.slot,
                site.leaf.text.to_string(),
            ));
        }
        assert_eq!(
            seen,
            [
                (1, 1, 1, "UNSTRUCTURED", "A".to_string()),
                (1, 1, 2, "UNSTRUCTURED", "B".to_string()),
                (1, 2, 1, "ADDITIONAL", "C".to_string()),
                (2, 2, 1, "UNSTRUCTURED", "D".to_string()),
            ],
            "a transaction with no remittance is walked past, not numbered"
        );
    }

    // ── amount details ───────────────────────────────────────────────────────

    /// The one boundary in the amount cursor worth a case of its own: where the
    /// entry's own blocks stop and the transactions' begin. All four fixed
    /// kinds, two proprietary ones, and then a block per transaction.
    #[test]
    fn model_the_amount_cursor_walks_the_entry_then_every_transaction() {
        let e = entry(
            "<AmtDtls>\
             <InstdAmt><Amt Ccy=\"EUR\">940.50</Amt></InstdAmt>\
             <TxAmt><Amt Ccy=\"CHF\">900.00</Amt></TxAmt>\
             <CntrValAmt><Amt Ccy=\"CHF\">900.00</Amt><CcyXchg><SrcCcy>EUR</SrcCcy>\
             <TrgtCcy>CHF</TrgtCcy><UnitCcy>EUR</UnitCcy><XchgRate>0.95695</XchgRate>\
             <CtrctId>FX-1</CtrctId><QtnDt>2026-08-19T08:55:00</QtnDt></CcyXchg></CntrValAmt>\
             <AnncdPstngAmt><Amt Ccy=\"CHF\">900.00</Amt></AnncdPstngAmt>\
             <PrtryAmt><Tp>CHARGEBASIS</Tp><Amt Ccy=\"CHF\">12.50</Amt></PrtryAmt>\
             <PrtryAmt><Tp>VATBASIS</Tp><Amt Ccy=\"CHF\">0.95</Amt></PrtryAmt>\
             </AmtDtls>\
             <NtryDtls><TxDtls><AmtDtls><InstdAmt><Amt Ccy=\"EUR\">313.50</Amt></InstdAmt>\
             </AmtDtls></TxDtls>\
             <TxDtls><AmtDtls><PrtryAmt><Tp>SETTLEMENTBASIS</Tp>\
             <Amt Ccy=\"CHF\">250.00</Amt></PrtryAmt></AmtDtls></TxDtls></NtryDtls>",
        );
        let mut cursor = AmountCursor::default();
        let mut seen = Vec::new();
        while let Some(site) = cursor.next(&e) {
            seen.push((
                site.scope,
                site.entry_details_index,
                site.transaction_index,
                site.kind,
                site.index,
                site.detail.tp.clone(),
            ));
        }
        assert_eq!(
            seen,
            [
                ("ENTRY", None, None, "INSTRUCTED", 1, None),
                ("ENTRY", None, None, "TRANSACTION", 2, None),
                ("ENTRY", None, None, "COUNTER_VALUE", 3, None),
                ("ENTRY", None, None, "ANNOUNCED_POSTING", 4, None),
                (
                    "ENTRY",
                    None,
                    None,
                    "PROPRIETARY",
                    5,
                    Some("CHARGEBASIS".to_string())
                ),
                (
                    "ENTRY",
                    None,
                    None,
                    "PROPRIETARY",
                    6,
                    Some("VATBASIS".to_string())
                ),
                ("TRANSACTION", Some(1), Some(1), "INSTRUCTED", 1, None),
                (
                    "TRANSACTION",
                    Some(1),
                    Some(2),
                    "PROPRIETARY",
                    1,
                    Some("SETTLEMENTBASIS".to_string())
                ),
            ]
        );
        let exchange = e
            .amt_dtls
            .as_ref()
            .and_then(|d| d.block(2))
            .and_then(|(_, detail)| detail.ccy_xchg.as_ref())
            .expect("the counter value carries an exchange");
        assert_eq!(exchange.xchg_rate.as_deref(), Some("0.95695"));
        assert_eq!(exchange.ctrct_id.as_deref(), Some("FX-1"));
    }

    /// An absent fixed slot consumes no index, and an entry with no `<AmtDtls>`
    /// of its own must not skip the transactions' blocks.
    #[test]
    fn model_an_absent_fixed_slot_takes_no_amount_index() {
        let e = entry(
            "<NtryDtls><TxDtls><AmtDtls><TxAmt><Amt Ccy=\"CHF\">1.00</Amt></TxAmt>\
             </AmtDtls></TxDtls></NtryDtls>",
        );
        let mut cursor = AmountCursor::default();
        let site = cursor.next(&e).expect("the transaction block is reached");
        assert_eq!(site.scope, "TRANSACTION");
        assert_eq!(site.kind, "TRANSACTION");
        assert_eq!(site.index, 1, "the absent InstdAmt is not index 1");
        assert!(cursor.next(&e).is_none());
    }

    /// A block that is present and empty is still a block: the message stated
    /// it, so it is a row with nullable facts.
    #[test]
    fn model_an_empty_amount_block_is_still_a_block() {
        let e = entry("<AmtDtls><TxAmt></TxAmt></AmtDtls>");
        let mut cursor = AmountCursor::default();
        let site = cursor.next(&e).expect("an empty block is a block");
        assert_eq!(site.kind, "TRANSACTION");
        assert!(site.detail.amt.is_none());
        assert!(site.detail.ccy_xchg.is_none());
        assert!(cursor.next(&e).is_none());
    }

    // ── balances ─────────────────────────────────────────────────────────────

    fn balance(body: &str) -> Balance {
        quick_xml::de::from_str(&format!("<Bal>{body}</Bal>")).expect("the balance parses")
    }

    /// Which vocabulary a balance type came from is a fact of its own: `OPBD`
    /// and a bank's `INTRADAY-PEAK` are not the same kind of thing, and one
    /// column could not say so.
    #[test]
    fn model_a_balance_reports_the_scheme_beside_the_value() {
        let coded = balance(
            "<Tp><CdOrPrtry><Cd>OPBD</Cd></CdOrPrtry><SubTp><Cd>INTM</Cd></SubTp></Tp>\
             <Amt Ccy=\"CHF\">10000.00</Amt><CdtDbtInd>CRDT</CdtDbtInd><Dt><Dt>2026-08-18</Dt></Dt>",
        );
        assert_eq!(coded.kind(), Some(("OPBD".to_string(), "CODE")));
        assert_eq!(coded.subkind(), Some(("INTM".to_string(), "CODE")));
        assert_eq!(
            coded.dt.as_ref().and_then(DateChoice::value).as_deref(),
            Some("2026-08-18")
        );

        let owned = balance(
            "<Tp><CdOrPrtry><Prtry>INTRADAY-PEAK</Prtry></CdOrPrtry>\
             <SubTp><Prtry>BANKAVL</Prtry></SubTp></Tp>\
             <Amt Ccy=\"CHF\">12500.00</Amt><CdtDbtInd>DBIT</CdtDbtInd>\
             <Dt><DtTm>2026-08-19T18:00:00</DtTm></Dt>",
        );
        assert_eq!(
            owned.kind(),
            Some(("INTRADAY-PEAK".to_string(), "PROPRIETARY"))
        );
        assert_eq!(
            owned.subkind(),
            Some(("BANKAVL".to_string(), "PROPRIETARY"))
        );
        assert_eq!(
            owned.dt.as_ref().and_then(DateChoice::value).as_deref(),
            Some("2026-08-19T18:00:00")
        );

        let bare = balance("<Tp><CdOrPrtry></CdOrPrtry></Tp><Amt Ccy=\"CHF\">1.00</Amt>");
        assert_eq!(bare.kind(), None, "an empty type invents no scheme");
        assert_eq!(bare.subkind(), None);
    }

    /// Money is exact or it is an error, at every site a row projects it.
    #[test]
    fn model_malformed_money_is_an_error_and_never_a_null() {
        let too_precise = entry("<Amt Ccy=\"CHF\">10.1234567</Amt>");
        let err = row_from_entry(&too_precise, &EntryCtx::default(), "bad.xml")
            .expect_err("seven fraction digits is not an ISO 20022 amount");
        assert!(err.contains("bad.xml"), "the file is named: {err}");
        assert!(err.contains("7 fraction digits"), "{err}");

        let amt = |value: &str| Amt {
            ccy: Some("CHF".to_string()),
            value: Some(value.to_string()),
        };
        money(Some(&amt("twelve")), "bad.xml").expect_err("not a number");
        money(Some(&amt("1.000.000")), "bad.xml").expect_err("not a number");
        // An element the sender wrote and left blank is absence, and the
        // currency it still carries is reported beside it.
        assert_eq!(
            money(Some(&amt("  ")), "bad.xml").expect("a blank amount is absent"),
            (None, Some("CHF".to_string()))
        );
        assert_eq!(
            money(None, "bad.xml").expect("an absent amount is absent"),
            (None, None)
        );
    }
}
