//! `quackiso_functions()` - what each function is for, as rows.
//!
//! The C API a loadable extension registers table functions through has no
//! description or example setter, so `duckdb_functions()` shows these functions
//! with both columns empty and a person or an agent has nothing to read but the
//! names. This table is the answer; ADR 0013. The drift check in
//! `scripts/check_hello_world.py` holds it to the catalog on every build: the
//! names must equal what LOAD adds to `duckdb_functions()`, and every example
//! must return rows.
//!
//! The description is the opening sentence of the function's section in
//! `docs/reference.md`, or its row in `docs/functions.md` when the section opens
//! with a column list. The paths are the generic ones of
//! `community-extension/description.yml`.

/// `(function_name, description, example)` for every registered function.
pub const FUNCTIONS: &[(&str, &str, &str)] = &[
    // ── cash management: one reader per grain a statement has ────────────────
    (
        "read_iso20022",
        "Reads camt.053 statements, camt.054 notifications and camt.052 reports, one row per booked entry.",
        "SELECT * FROM read_iso20022('statements/*.xml')",
    ),
    (
        "read_camt_transactions",
        "One row per `Ntry/NtryDtls/TxDtls` of a camt.052, camt.053 or camt.054.",
        "SELECT * FROM read_camt_transactions('statements/*.xml')",
    ),
    (
        "read_camt_balances",
        "One row per `<Bal>` directly under a camt.052 `Rpt` or a camt.053 `Stmt`.",
        "SELECT * FROM read_camt_balances('statements/*.xml')",
    ),
    (
        "read_camt_amount_details",
        "One row per amount block inside an entry-level or transaction-level `<AmtDtls>`: `InstdAmt`, `TxAmt`, `CntrValAmt`, `AnncdPstngAmt` and every `PrtryAmt`.",
        "SELECT * FROM read_camt_amount_details('statements/*.xml')",
    ),
    (
        "read_camt_remittance",
        "One row per non-empty remittance text leaf under a transaction: every `Ustrd`, every `Strd/RfrdDocInf/Nb`, every `Strd/CdtrRefInf/Ref`, every `Strd/AddtlRmtInf`.",
        "SELECT * FROM read_camt_remittance('statements/*.xml')",
    ),
    (
        "read_camt_statements",
        "One row per `Stmt` of a camt.053, `Rpt` of a camt.052 or `Ntfctn` of a camt.054: the header the statement states about itself, beside the number of entries and balances it holds.",
        "SELECT * FROM read_camt_statements('statements/*.xml')",
    ),
    // ── pacs and pain: payments, their status, returns and mandates ──────────
    (
        "read_pacs008",
        "One row per `CdtTrfTxInf` of a pacs.008 FI-to-FI customer credit transfer, the ISO 20022 MT103.",
        "SELECT * FROM read_pacs008('payments/*.xml')",
    ),
    (
        "read_pacs004",
        "One row per `TxInf` of a pacs.004 payment return, settled money coming back.",
        "SELECT * FROM read_pacs004('payments/*.xml')",
    ),
    (
        "read_pacs002",
        "One row per status statement of a pacs.002 FI-to-FI payment status report.",
        "SELECT * FROM read_pacs002('payments/*.xml')",
    ),
    (
        "read_pacs028",
        "One row per status request of a pacs.028 FI-to-FI payment status request, asking where a payment already sent got to.",
        "SELECT * FROM read_pacs028('payments/*.xml')",
    ),
    (
        "read_pain001",
        "One row per transaction of a pain.001 credit transfer initiation.",
        "SELECT * FROM read_pain001('payments/*.xml')",
    ),
    (
        "read_pain002",
        "One row per status statement of a pain.002 customer payment status report.",
        "SELECT * FROM read_pain002('payments/*.xml')",
    ),
    (
        "read_pain008",
        "One row per collection of a pain.008 direct debit initiation, where the creditor pulls.",
        "SELECT * FROM read_pain008('payments/*.xml')",
    ),
    (
        "read_pain009",
        "One row per `Mndt` of a pain.009 mandate initiation, the mandate a pain.008 pulls against.",
        "SELECT * FROM read_pain009('payments/*.xml')",
    ),
    (
        "read_pain010",
        "One row per amendment of a pain.010 mandate amendment.",
        "SELECT * FROM read_pain010('payments/*.xml')",
    ),
    (
        "read_pain011",
        "One row per cancellation of a pain.011 mandate cancellation.",
        "SELECT * FROM read_pain011('payments/*.xml')",
    ),
    (
        "read_pain012",
        "One row per answer of a pain.012 mandate acceptance report, the answer to a pain.009, pain.010 or pain.011.",
        "SELECT * FROM read_pain012('payments/*.xml')",
    ),
    (
        "read_pain013",
        "One row per `CdtTrfTx` of a pain.013 creditor payment activation request, a request to pay.",
        "SELECT * FROM read_pain013('payments/*.xml')",
    ),
    (
        "read_pain014",
        "One row per status statement of a pain.014 creditor payment activation request status report.",
        "SELECT * FROM read_pain014('payments/*.xml')",
    ),
    (
        "read_pacs009",
        "One row per `CdtTrfTxInf` of a pacs.009 financial institution transfer, the ISO 20022 MT202 and MT202COV.",
        "SELECT * FROM read_pacs009('payments/*.xml')",
    ),
    (
        "read_pacs010",
        "One row per `DrctDbtTxInf` of a pacs.010 FI-to-FI direct debit where both sides are banks.",
        "SELECT * FROM read_pacs010('payments/*.xml')",
    ),
    (
        "read_pacs003",
        "One row per `DrctDbtTxInf` of a pacs.003 FI-to-FI direct debit, the interbank leg of a pain.008.",
        "SELECT * FROM read_pacs003('payments/*.xml')",
    ),
    (
        "read_pacs007",
        "One row per `TxInf` of a pacs.007 payment reversal, the sender taking a payment back.",
        "SELECT * FROM read_pacs007('payments/*.xml')",
    ),
    // ── camt exceptions and investigations ───────────────────────────────────
    (
        "read_camt056",
        "One row per cancellation statement of a camt.056 payment cancellation request.",
        "SELECT * FROM read_camt056('payments/*.xml')",
    ),
    (
        "read_camt057",
        "One row per `Itm` of a camt.057 notification to receive, money on its way in.",
        "SELECT * FROM read_camt057('payments/*.xml')",
    ),
    (
        "read_camt055",
        "One row per cancellation statement of a camt.055 customer payment cancellation request.",
        "SELECT * FROM read_camt055('payments/*.xml')",
    ),
    (
        "read_camt029",
        "One row per statement of a camt.029 resolution of investigation, the answer to a camt.056.",
        "SELECT * FROM read_camt029('payments/*.xml')",
    ),
    (
        "read_camt027",
        "One row per claim of a camt.027 claim of non-receipt, for money that never arrived.",
        "SELECT * FROM read_camt027('payments/*.xml')",
    ),
    (
        "read_camt028",
        "One row per answer of a camt.028 additional payment information message.",
        "SELECT * FROM read_camt028('payments/*.xml')",
    ),
    (
        "read_camt030",
        "One row per notification of a camt.030 notification of case assignment.",
        "SELECT * FROM read_camt030('payments/*.xml')",
    ),
    (
        "read_camt031",
        "One row per rejection of a camt.031 reject investigation.",
        "SELECT * FROM read_camt031('payments/*.xml')",
    ),
    (
        "read_camt036",
        "One row per response of a camt.036 debit authorisation response.",
        "SELECT * FROM read_camt036('payments/*.xml')",
    ),
    (
        "read_camt037",
        "One row per request of a camt.037 debit authorisation request, asking whether a debit may be taken back.",
        "SELECT * FROM read_camt037('payments/*.xml')",
    ),
    (
        "read_camt087",
        "One row per request of a camt.087 request to modify a payment.",
        "SELECT * FROM read_camt087('payments/*.xml')",
    ),
    // ── SWIFT MT ─────────────────────────────────────────────────────────────
    (
        "read_mt101",
        "One row per transaction of a SWIFT MT101 request for transfer, the FIN original of pain.001.",
        "SELECT * FROM read_mt101('mt/*.txt')",
    ),
    (
        "read_mt104",
        "One row per transaction of a SWIFT MT104 direct debit and request for debit transfer, the FIN side of pain.008.",
        "SELECT * FROM read_mt104('mt/*.txt')",
    ),
    (
        "read_mt103",
        "One row per message of a SWIFT MT103 single customer credit transfer.",
        "SELECT * FROM read_mt103('mt/*.txt')",
    ),
    (
        "read_mt202",
        "One row per message of a SWIFT MT202 or MT202COV financial institution transfer.",
        "SELECT * FROM read_mt202('mt/*.txt')",
    ),
    (
        "read_mt940",
        "One row per `:61:` line of a SWIFT MT940 customer statement.",
        "SELECT * FROM read_mt940('mt/*.txt')",
    ),
    (
        "read_mt942",
        "One row per `:61:` line of a SWIFT MT942 interim transaction report.",
        "SELECT * FROM read_mt942('mt/*.txt')",
    ),
    // ── inspection ───────────────────────────────────────────────────────────
    (
        "sniff_iso20022",
        "The inventory function: point it at a directory before choosing a reader.",
        "SELECT * FROM sniff_iso20022('inbox/*')",
    ),
    (
        "audit_addresses",
        "One row per party of any message quackiso reads, ISO 20022 or SWIFT MT, beside the shape of its postal address.",
        "SELECT * FROM audit_addresses('payments/*')",
    ),
    (
        "quackiso_functions",
        "One row per function this extension registers, with a one-sentence description and a runnable example.",
        "SELECT * FROM quackiso_functions()",
    ),
];
