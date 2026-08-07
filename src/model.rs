//! Serde model for the subset of camt.053 (bank-to-customer statement) that the
//! reader flattens into rows. Every field is optional: real-world messages omit
//! optional elements constantly, and a reader that panics on a missing tag is
//! useless. Missing -> None -> SQL NULL.
//!
//! Only the `<Ntry>` subtree and its children are modelled. There is no struct
//! for the document or the statement, because nothing deserializes one: the
//! reader walks to each entry as events and hands only that subtree to serde.
//!
//! quick-xml's serde matches on local tag names, so the ISO 20022 default
//! namespace (`xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.xx"`) needs
//! no special handling here.

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
    #[serde(rename = "Sts")]
    pub sts: Option<CodeOrText>,
    #[serde(rename = "BookgDt")]
    pub bookg_dt: Option<DateChoice>,
    #[serde(rename = "ValDt")]
    pub val_dt: Option<DateChoice>,
    #[serde(rename = "AcctSvcrRef")]
    pub acct_svcr_ref: Option<String>,
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
    #[serde(rename = "TxDtls", default)]
    pub tx_dtls: Vec<TxDtls>,
}

#[derive(Debug, Deserialize)]
pub struct TxDtls {
    #[serde(rename = "Refs")]
    pub refs: Option<Refs>,
    #[serde(rename = "RltdPties")]
    pub rltd_pties: Option<RltdPties>,
    #[serde(rename = "RmtInf")]
    pub rmt_inf: Option<RmtInf>,
}

#[derive(Debug, Deserialize)]
pub struct Refs {
    #[serde(rename = "EndToEndId")]
    pub end_to_end_id: Option<String>,
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

/// One flattened output row: a single booked entry (`Ntry`) with its statement
/// and first-transaction context resolved. This is the grain of `read_iso20022`.
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
    pub end_to_end_id: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty_iban: Option<String>,
    pub remittance_info: Option<String>,
    pub source_file: Option<String>,
}

/// Build one output row from a single entry plus its statement/message context.
/// Shared by the eager `flatten` (used in tests) and the streaming reader.
///
/// Fails rather than nulling a malformed amount: a NULL would disappear from a
/// `SUM` and hand back a plausible wrong total.
pub fn row_from_entry(
    ntry: &Ntry,
    msg_id: &Option<String>,
    account_iban: &Option<String>,
    statement_id: &Option<String>,
    source_file: &str,
) -> Result<Row, String> {
    let cdt_dbt = ntry.cdt_dbt_ind.clone();
    // Counterparty is the other side of the flow: money out (DBIT) -> the
    // creditor is who we paid; money in (CRDT) -> the debtor.
    let first_tx = ntry.ntry_dtls.first().and_then(|d| d.tx_dtls.first());
    let (cp_name, cp_iban) = counterparty(cdt_dbt.as_deref(), first_tx);

    Ok(Row {
        msg_id: msg_id.clone(),
        account_iban: account_iban.clone(),
        statement_id: statement_id.clone(),
        entry_ref: ntry.ntry_ref.clone(),
        amount: decimal::scaled_opt(ntry.amt.as_ref().and_then(|a| a.value.as_ref()))
            .map_err(|e| format!("{source_file}: {e}"))?,
        currency: ntry.amt.as_ref().and_then(|a| a.ccy.clone()),
        credit_debit: cdt_dbt,
        status: ntry.sts.as_ref().and_then(|s| s.value()),
        booking_date: ntry.bookg_dt.as_ref().and_then(|d| d.value()),
        value_date: ntry.val_dt.as_ref().and_then(|d| d.value()),
        bank_ref: ntry.acct_svcr_ref.clone(),
        end_to_end_id: first_tx
            .and_then(|t| t.refs.as_ref())
            .and_then(|r| r.end_to_end_id.clone()),
        counterparty_name: cp_name,
        counterparty_iban: cp_iban,
        remittance_info: first_tx.and_then(remittance),
        source_file: Some(source_file.to_string()),
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

/// Free-text remittance if present, else the structured creditor reference or
/// additional remittance text. Corporate statements often carry only Strd.
fn remittance(tx: &TxDtls) -> Option<String> {
    let rmt = tx.rmt_inf.as_ref()?;
    if !rmt.ustrd.is_empty() {
        return Some(rmt.ustrd.join(" "));
    }
    for s in &rmt.strd {
        if let Some(r) = s.cdtr_ref_inf.as_ref().and_then(|c| c.reference.clone()) {
            return Some(r);
        }
        if !s.addtl.is_empty() {
            return Some(s.addtl.join(" "));
        }
    }
    None
}
