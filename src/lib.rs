//! quackiso - query ISO 20022 and SWIFT MT financial messages as SQL in DuckDB.
//!
//! Thirty-nine streaming readers, a sniffer to route files to them, and an
//! address audit that reads both wire formats:
//!
//! * `read_iso20022(path)` — cash management: camt.053 statements, camt.054
//!   notifications, camt.052 reports. One row per booked entry, with the counts
//!   that say how many transactions and remittance leaves are under it, so the
//!   convenience columns can be NULL where one payment cannot answer for three.
//! * `read_camt_transactions(path)` - the same three messages at transaction
//!   grain. One row per `Ntry/NtryDtls/TxDtls`, with the batch it was posted
//!   under and no fallback to the entry's values.
//! * `read_camt_balances(path)` - the same three at balance grain. One row per
//!   `<Bal>`, which is what a statement of no movements consists of.
//! * `read_camt_amount_details(path)` - the same three at amount-block grain.
//!   One row per `<AmtDtls>` block, with the currency exchange beside it.
//! * `read_camt_remittance(path)` - the same three at remittance grain. One row
//!   per non-empty text leaf, so two invoice numbers are two rows.
//! * `read_pacs008(path)` — FI-to-FI customer credit transfers (the ISO 20022
//!   replacement for SWIFT MT103). One row per transaction.
//! * `read_pacs009(path)` — financial institution transfers (MT202/MT202COV):
//!   banks moving money between themselves. One row per transaction, with the
//!   COV underlying customer transfer beside the interbank leg.
//! * `read_pacs003(path)` — FI-to-FI customer direct debits: the interbank leg
//!   of a pain.008 collection, with the mandate travelling beside the money.
//! * `read_pacs010(path)` - FI-to-FI direct debits: one bank collecting from
//!   another, with the creditor carried down from its `CdtInstr` instruction.
//! * `read_pacs007(path)` — payment reversals: the sender taking a settled
//!   payment back, typically a direct debit collected in error.
//! * `read_pacs004(path)` — payment returns: settled money coming back. One row
//!   per returned transaction, with the original amount beside the returned one.
//! * `read_pacs002(path)` — FI-to-FI payment status reports. One row per status
//!   statement, at batch or transaction level.
//! * `read_pacs028(path)` - FI-to-FI payment status requests: asking another
//!   bank for the status of a payment already sent. One row per status
//!   request, at group or transaction grain.
//! * `read_pain001(path)` — customer credit transfer initiation. One row per
//!   transaction, with the payer carried down from its `PmtInf` group.
//! * `read_pain002(path)` — customer payment status reports. One row per status
//!   statement, at whichever of the three levels the bank stated it.
//! * `read_pain008(path)` — direct debit initiation: the creditor pulls. One
//!   row per collection, with the collector carried down from its `PmtInf`
//!   group and the mandate beside the money.
//! * `read_pain009(path)` — mandate initiation: the creditor asking for the
//!   authorisation a direct debit needs. One row per mandate.
//! * `read_pain010(path)` — mandate amendment: the new state of a mandate
//!   beside the id of the one it changes. One row per amendment.
//! * `read_pain011(path)` — mandate cancellation. One row per cancellation;
//!   the id-only form is a complete record.
//! * `read_pain012(path)` — mandate acceptance reports: the answer to a
//!   pain.009, pain.010 or pain.011. One row per answer.
//! * `read_pain013(path)` - creditor payment activation requests: a request to
//!   pay, before any money moves. One row per requested transfer.
//! * `read_pain014(path)` - the debtor side's answer to a pain.013. One row per
//!   status statement, at whichever of the three levels it was stated.
//! * `read_camt056(path)` — payment cancellation requests. One row per
//!   cancellation statement; a whole-batch cancellation is a row too.
//! * `read_camt055(path)` — customer payment cancellation requests: the
//!   customer-side camt.056, with the pain-side payment-info level.
//! * `read_camt029(path)` — resolutions of investigation: the answer to a
//!   cancellation. One row per statement; most real files answer at message
//!   level only.
//! * `read_camt027(path)` — claims of non-receipt: the money never arrived.
//!   One row per claim.
//! * `read_camt028(path)` — additional payment information: the detail an
//!   investigation asked for. One row per answer.
//! * `read_camt030(path)` — notifications of case assignment, carrying two
//!   party pairs that need not agree. One row per notification.
//! * `read_camt031(path)` — rejected investigations: the case will not be
//!   worked, and why. One row per rejection.
//! * `read_camt037(path)` — debit authorisation requests: may I take this back
//!   off your account? One row per request, with the amount asked for beside
//!   the original.
//! * `read_camt036(path)` — the customer's answer to that request. One row per
//!   response.
//! * `read_camt087(path)` — requests to modify a payment rather than cancel
//!   it. One row per request, the modification beside the original.
//! * `read_camt057(path)` - notifications to receive: money on its way in and
//!   not yet booked. One row per expected item.
//! * `read_mt101(path)` - MT101 requests for transfer, the FIN original of
//!   pain.001. One row per transaction, header repeated on each.
//! * `read_mt103(path)` - SWIFT MT103 single customer credit transfers, the FIN
//!   original of pacs.008. One row per message.
//! * `read_mt104(path)` - MT104 direct debits and requests for debit transfer,
//!   the FIN side of pain.008. One row per transaction, with the batch total the
//!   settlement sequence states carried onto each.
//! * `read_mt202(path)` - MT202 and MT202COV financial institution transfers.
//!   One row per message, the cover's underlying customer transfer beside the
//!   interbank leg.
//! * `read_mt940(path)` - MT940 customer statements. One row per `:61:`
//!   statement line, with the account and all four balances carried onto it.
//! * `read_mt942(path)` - MT942 interim transaction reports: what has happened
//!   since the last statement. One row per `:61:` line.
//! * `sniff_iso20022(path)` — inventory before reading: one row per file with
//!   the detected message type, the reader that covers it, and the count of
//!   record elements a reader would turn into rows. Content problems land in an
//!   `error` column; they never abort the scan.
//! * `audit_addresses(path)` - every party of every message beside the shape of
//!   its postal address: STRUCTURED, HYBRID, UNSTRUCTURED or NONE, the counts
//!   behind that, and a `finding` naming what the 14 November 2026 CBPR+ rule
//!   would refuse. One row per party occurrence, so a folder groups by role, by
//!   country and by format in one query.
//!
//! The sniffer recognises SWIFT MT too, by the block structure rather than by a
//! namespace: an MT file reports an `mt.nnn` family, a NULL `namespace`, and a
//! `records` count that is the rows its reader would return.
//!
//! `bind` only resolves the file list; parsing happens in `func`, which pulls the
//! next vector-sized batch on demand, so the peak is one batch plus the largest
//! single subtree, never the file: 1.7 GB reads in under 2 MB of live heap,
//! measured in `src/membound.rs`. Paths are local, globs are expanded, and a
//! gzipped file is read as the statement inside it.
//!
//! Reading through DuckDB's own filesystem (`s3://`, `https://`) is deliberately
//! absent rather than half-working; `docs/adr/0002-no-remote-paths.md` records the
//! blocker and what it would take.

pub(crate) mod addresses;
pub(crate) mod camt;
pub(crate) mod camt027;
pub(crate) mod camt028;
pub(crate) mod camt029;
pub(crate) mod camt030;
pub(crate) mod camt031;
pub(crate) mod camt036;
pub(crate) mod camt037;
pub(crate) mod camt055;
pub(crate) mod camt056;
pub(crate) mod camt057;
pub(crate) mod camt087;
pub(crate) mod camt_amount_details;
pub(crate) mod camt_balances;
pub(crate) mod camt_remittance;
pub(crate) mod camt_transactions;
pub(crate) mod container;
pub(crate) mod decimal;
#[cfg(test)]
pub(crate) mod membound;
pub(crate) mod model;
pub(crate) mod mt;
pub(crate) mod mt101;
pub(crate) mod mt103;
pub(crate) mod mt104;
pub(crate) mod mt202;
pub(crate) mod mt940;
pub(crate) mod mt942;
pub(crate) mod pacs002;
pub(crate) mod pacs003;
pub(crate) mod pacs004;
pub(crate) mod pacs007;
pub(crate) mod pacs008;
pub(crate) mod pacs009;
pub(crate) mod pacs010;
pub(crate) mod pacs028;
pub(crate) mod pain001;
pub(crate) mod pain002;
pub(crate) mod pain008;
pub(crate) mod pain009;
pub(crate) mod pain010;
pub(crate) mod pain011;
pub(crate) mod pain012;
pub(crate) mod pain013;
pub(crate) mod pain014;
pub(crate) mod sniff;
pub(crate) mod stream;
pub(crate) mod temporal;
pub(crate) mod wire;

use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use flate2::read::MultiGzDecoder;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::{
    error::Error,
    fs::File,
    io::{BufReader, Chain, Cursor, Read, Take},
};

use addresses::{AddrRow, Addresses};
use camt027::{ClaimRow, ClaimStream};
use camt028::{AddtlInfRow, AddtlInfStream};
use camt029::{RoiRow, RoiStream};
use camt030::{CaseNtfctnRow, CaseNtfctnStream};
use camt031::{RjctRow, RjctStream};
use camt036::{DbtRspnRow, DbtRspnStream};
use camt037::{DbtReqRow, DbtReqStream};
use camt055::{CclRow, CclStream};
use camt056::{CxlRow, CxlStream};
use camt057::{NtfctnRow, NtfctnStream};
use camt087::{ModfyRow, ModfyStream};
use camt_amount_details::{AmountDetailRow, AmountDetailStream};
use camt_balances::{BalanceRow, BalanceStream};
use camt_remittance::{RemittanceRow, RemittanceStream};
use camt_transactions::{TransactionRow, TransactionStream};
use container::ContainerKind;
use model::Row;
use pacs002::{RptRow, RptStream};
use pacs003::{DdiRow, DdiStream};
use pacs004::{RtrRow, RtrStream};
use pacs007::{RvslRow, RvslStream};
use pacs008::{PacsRow, TxStream};
use pacs009::{FiRow, FiStream};
use pacs010::{FiDdRow, FiDdStream};
use pacs028::{StsReqRow, StsReqStream};
use pain001::{PainRow, PainStream};
use pain002::{StsRow, StsStream};
use pain008::{DdRow, DdStream};
use pain009::{MndtRow, MndtStream};
use pain010::{AmdmntRow, AmdmntStream};
use pain011::{MndtCxlRow, MndtCxlStream};
use pain012::{AccptncRow, AccptncStream};
use pain013::{ActvtnRow, ActvtnStream};
use pain014::{ActvtnStsRow, ActvtnStsStream};
use sniff::{shape_of, Shape, SniffRow, SniffStream, PREFIX_BYTES};
use stream::EntryStream;
// SWIFT MT: not ISO 20022, so they sort after it rather than into it.
use mt101::{Mt101Row, Mt101Stream};
use mt103::{Mt103Row, Mt103Stream};
use mt104::{Mt104Row, Mt104Stream};
use mt202::{Mt202Row, Mt202Stream};
use mt940::{Mt940Row, Mt940Stream};
use mt942::{Mt942Row, Mt942Stream};

/// DuckDB's standard vector size. Rows are emitted in chunks of this many.
const VECTOR_SIZE: usize = 2048;

/// Byte source for a scan. Buffered because the readers pull small XML events.
type Source = BufReader<Input>;

/// A byte source that names itself. A read can fail in the middle of a stream --
/// a gzip member cut short above all -- and `quick-xml` passes that up as a bare
/// `unexpected end of file`, which over a glob of a year's statements says
/// nothing about which file to look at. Every error out of here carries the path,
/// the way `File::open` failures already did.
struct Input {
    name: Box<str>,
    replay: Cursor<Vec<u8>>,
    bytes: Bytes,
    /// What the guard decided this file is, for the one function that reads both
    /// and has to know which walk to open. None until a guard sets it.
    shape: Option<Shape>,
}

/// A statement arrives as XML, or as the same XML gzipped -- banks ship both,
/// and a day's dump is often members appended one per delivery. Either way the
/// readers see one buffered byte source and nothing about compression.
enum Bytes {
    Plain(Peeked),
    Gz(MultiGzDecoder<Peeked>),
}

/// The file behind the bytes the magic check already consumed. Handing them back
/// costs nothing and asks nothing of the source: a statement may arrive down a
/// FIFO, and a FIFO cannot seek.
type Peeked = Chain<Take<Cursor<[u8; 2]>>, File>;

impl Read for Input {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.replay.position() < self.replay.get_ref().len() as u64 {
            return self.replay.read(buf);
        }
        match &mut self.bytes {
            Bytes::Plain(file) => file.read(buf),
            Bytes::Gz(gz) => gz.read(buf),
        }
        // Allocates on failure and never on the way through.
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", self.name)))
    }
}

fn open_source(path: &str) -> Result<Input, Box<dyn Error>> {
    let mut file = File::open(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let (magic, have) = peek(&mut file).map_err(|e| format!("cannot read {path}: {e}"))?;
    let peeked = Cursor::new(magic).take(have as u64).chain(file);
    let bytes = if have == GZIP_MAGIC.len() && magic == GZIP_MAGIC {
        // MultiGzDecoder, not GzDecoder: concatenated members are one stream,
        // and stopping after the first would silently truncate the statement.
        Bytes::Gz(MultiGzDecoder::new(peeked))
    } else {
        Bytes::Plain(peeked)
    };
    Ok(Input {
        name: path.into(),
        replay: Cursor::new(Vec::new()),
        bytes,
        shape: None,
    })
}

/// Gzip announces itself in its first two bytes. This reader decides what a file
/// is by reading it rather than by trusting its name -- that is what
/// `sniff_iso20022` exists for -- so compression is settled the same way:
/// `.xml.gz`, `.gz`, and a gzipped file still called `.xml` all read alike.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The first two bytes, and how many of them a short file actually had.
fn peek(file: &mut File) -> std::io::Result<([u8; 2], usize)> {
    let mut magic = [0u8; 2];
    let mut have = 0;
    while have < magic.len() {
        match file.read(&mut magic[have..])? {
            0 => break,
            n => have += n,
        }
    }
    Ok((magic, have))
}

// ── shared scan machinery ────────────────────────────────────────────────────

/// The prefix, the shape it says the file is, and the bytes put back. Every
/// reader here needs the first two; only the caller decides what to do with a
/// shape it does not want.
fn shape_prefix(input: &mut Input) -> Result<Shape, Box<dyn Error>> {
    let mut prefix = Vec::new();
    (&mut *input).take(PREFIX_BYTES).read_to_end(&mut prefix)?;
    let shape = shape_of(&prefix);
    input.replay = Cursor::new(prefix);
    input.shape = Some(shape);
    Ok(shape)
}

/// A named transport container, refused by every public path with the same
/// sentence. Shared so a caller cannot get one account of a ZIP from the
/// sniffer and a different one from a reader.
fn refuse_container(kind: ContainerKind, name: &str) -> Box<dyn Error> {
    format!("{name}: {}", kind.reason()).into()
}

fn guard_xml_prefix(input: &mut Input) -> Result<(), Box<dyn Error>> {
    match shape_prefix(input)? {
        Shape::Container(kind) => Err(refuse_container(kind, &input.name)),
        Shape::Mt => Err(format!("{}: not XML: SWIFT MT marker before markup", input.name).into()),
        Shape::NotXml => {
            Err(format!("{}: not XML: no markup in the first 64 KiB", input.name).into())
        }
        Shape::Xml => Ok(()),
    }
}

/// The address audit reads both wire formats, so its guard refuses only a file
/// that is neither: an MT marker is a shape it handles rather than a rejection.
fn guard_message_prefix(input: &mut Input) -> Result<(), Box<dyn Error>> {
    match shape_prefix(input)? {
        Shape::Container(kind) => Err(refuse_container(kind, &input.name)),
        Shape::NotXml => Err(format!(
            "{}: neither XML nor SWIFT MT in the first 64 KiB",
            input.name
        )
        .into()),
        Shape::Mt | Shape::Xml => Ok(()),
    }
}

/// The MT readers accept whatever their own framer accepts - a bare statement
/// body, an ACK envelope, a file with markup after the messages - so their
/// guard subtracts nothing from that. It refuses one thing: a container, which
/// no framer can read and which used to come back as a partial parse of the
/// archive's own header bytes.
fn guard_container_only(input: &mut Input) -> Result<(), Box<dyn Error>> {
    match shape_prefix(input)? {
        Shape::Container(kind) => Err(refuse_container(kind, &input.name)),
        Shape::Mt | Shape::NotXml | Shape::Xml => Ok(()),
    }
}

/// A streaming reader over one file, yielding flattened rows.
trait RowStream: Sized {
    type Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_xml_prefix(input)
    }
    fn open(source: Source, name: &str) -> Self;
    fn next_row(&mut self) -> Result<Option<Self::Row>, Box<dyn Error>>;

    fn from_input(mut input: Input, name: &str) -> Result<Self, Box<dyn Error>> {
        Self::guard(&mut input)?;
        Ok(Self::open(BufReader::with_capacity(64 * 1024, input), name))
    }
}

impl RowStream for EntryStream<Source> {
    type Row = Row;
    fn open(source: Source, name: &str) -> Self {
        EntryStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Row>, Box<dyn Error>> {
        EntryStream::next_row(self)
    }
}

// The four supplementary camt readers. They share `camt::StatementRecordStream`
// and therefore the same wrong-file refusal, so a caller pointing any of them
// at a pain.001 gets the sentence `read_iso20022` gives.
impl RowStream for TransactionStream<Source> {
    type Row = TransactionRow;
    fn open(source: Source, name: &str) -> Self {
        TransactionStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<TransactionRow>, Box<dyn Error>> {
        TransactionStream::next_row(self)
    }
}

impl RowStream for BalanceStream<Source> {
    type Row = BalanceRow;
    fn open(source: Source, name: &str) -> Self {
        BalanceStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<BalanceRow>, Box<dyn Error>> {
        BalanceStream::next_row(self)
    }
}

impl RowStream for AmountDetailStream<Source> {
    type Row = AmountDetailRow;
    fn open(source: Source, name: &str) -> Self {
        AmountDetailStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<AmountDetailRow>, Box<dyn Error>> {
        AmountDetailStream::next_row(self)
    }
}

impl RowStream for RemittanceStream<Source> {
    type Row = RemittanceRow;
    fn open(source: Source, name: &str) -> Self {
        RemittanceStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RemittanceRow>, Box<dyn Error>> {
        RemittanceStream::next_row(self)
    }
}

impl RowStream for TxStream<Source> {
    type Row = PacsRow;
    fn open(source: Source, name: &str) -> Self {
        TxStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<PacsRow>, Box<dyn Error>> {
        TxStream::next_row(self)
    }
}

impl RowStream for PainStream<Source> {
    type Row = PainRow;
    fn open(source: Source, name: &str) -> Self {
        PainStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<PainRow>, Box<dyn Error>> {
        PainStream::next_row(self)
    }
}

impl RowStream for RtrStream<Source> {
    type Row = RtrRow;
    fn open(source: Source, name: &str) -> Self {
        RtrStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RtrRow>, Box<dyn Error>> {
        RtrStream::next_row(self)
    }
}

impl RowStream for StsStream<Source> {
    type Row = StsRow;
    fn open(source: Source, name: &str) -> Self {
        StsStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<StsRow>, Box<dyn Error>> {
        StsStream::next_row(self)
    }
}

impl RowStream for RptStream<Source> {
    type Row = RptRow;
    fn open(source: Source, name: &str) -> Self {
        RptStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RptRow>, Box<dyn Error>> {
        RptStream::next_row(self)
    }
}

impl RowStream for CxlStream<Source> {
    type Row = CxlRow;
    fn open(source: Source, name: &str) -> Self {
        CxlStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<CxlRow>, Box<dyn Error>> {
        CxlStream::next_row(self)
    }
}

impl RowStream for NtfctnStream<Source> {
    type Row = NtfctnRow;
    fn open(source: Source, name: &str) -> Self {
        NtfctnStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<NtfctnRow>, Box<dyn Error>> {
        NtfctnStream::next_row(self)
    }
}

impl RowStream for DdStream<Source> {
    type Row = DdRow;
    fn open(source: Source, name: &str) -> Self {
        DdStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<DdRow>, Box<dyn Error>> {
        DdStream::next_row(self)
    }
}

impl RowStream for RoiStream<Source> {
    type Row = RoiRow;
    fn open(source: Source, name: &str) -> Self {
        RoiStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RoiRow>, Box<dyn Error>> {
        RoiStream::next_row(self)
    }
}

impl RowStream for DdiStream<Source> {
    type Row = DdiRow;
    fn open(source: Source, name: &str) -> Self {
        DdiStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<DdiRow>, Box<dyn Error>> {
        DdiStream::next_row(self)
    }
}

impl RowStream for FiStream<Source> {
    type Row = FiRow;
    fn open(source: Source, name: &str) -> Self {
        FiStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<FiRow>, Box<dyn Error>> {
        FiStream::next_row(self)
    }
}

impl RowStream for FiDdStream<Source> {
    type Row = FiDdRow;
    fn open(source: Source, name: &str) -> Self {
        FiDdStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<FiDdRow>, Box<dyn Error>> {
        FiDdStream::next_row(self)
    }
}

impl RowStream for CclStream<Source> {
    type Row = CclRow;
    fn open(source: Source, name: &str) -> Self {
        CclStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<CclRow>, Box<dyn Error>> {
        CclStream::next_row(self)
    }
}

impl RowStream for RvslStream<Source> {
    type Row = RvslRow;
    fn open(source: Source, name: &str) -> Self {
        RvslStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RvslRow>, Box<dyn Error>> {
        RvslStream::next_row(self)
    }
}

impl RowStream for StsReqStream<Source> {
    type Row = StsReqRow;
    fn open(source: Source, name: &str) -> Self {
        StsReqStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<StsReqRow>, Box<dyn Error>> {
        StsReqStream::next_row(self)
    }
}

impl RowStream for MndtStream<Source> {
    type Row = MndtRow;
    fn open(source: Source, name: &str) -> Self {
        MndtStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<MndtRow>, Box<dyn Error>> {
        MndtStream::next_row(self)
    }
}

impl RowStream for AmdmntStream<Source> {
    type Row = AmdmntRow;
    fn open(source: Source, name: &str) -> Self {
        AmdmntStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<AmdmntRow>, Box<dyn Error>> {
        AmdmntStream::next_row(self)
    }
}

impl RowStream for MndtCxlStream<Source> {
    type Row = MndtCxlRow;
    fn open(source: Source, name: &str) -> Self {
        MndtCxlStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<MndtCxlRow>, Box<dyn Error>> {
        MndtCxlStream::next_row(self)
    }
}

impl RowStream for AccptncStream<Source> {
    type Row = AccptncRow;
    fn open(source: Source, name: &str) -> Self {
        AccptncStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<AccptncRow>, Box<dyn Error>> {
        AccptncStream::next_row(self)
    }
}

impl RowStream for ActvtnStream<Source> {
    type Row = ActvtnRow;
    fn open(source: Source, name: &str) -> Self {
        ActvtnStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<ActvtnRow>, Box<dyn Error>> {
        ActvtnStream::next_row(self)
    }
}

impl RowStream for ActvtnStsStream<Source> {
    type Row = ActvtnStsRow;
    fn open(source: Source, name: &str) -> Self {
        ActvtnStsStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<ActvtnStsRow>, Box<dyn Error>> {
        ActvtnStsStream::next_row(self)
    }
}

impl RowStream for ClaimStream<Source> {
    type Row = ClaimRow;
    fn open(source: Source, name: &str) -> Self {
        ClaimStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<ClaimRow>, Box<dyn Error>> {
        ClaimStream::next_row(self)
    }
}

impl RowStream for AddtlInfStream<Source> {
    type Row = AddtlInfRow;
    fn open(source: Source, name: &str) -> Self {
        AddtlInfStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<AddtlInfRow>, Box<dyn Error>> {
        AddtlInfStream::next_row(self)
    }
}

impl RowStream for CaseNtfctnStream<Source> {
    type Row = CaseNtfctnRow;
    fn open(source: Source, name: &str) -> Self {
        CaseNtfctnStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<CaseNtfctnRow>, Box<dyn Error>> {
        CaseNtfctnStream::next_row(self)
    }
}

impl RowStream for RjctStream<Source> {
    type Row = RjctRow;
    fn open(source: Source, name: &str) -> Self {
        RjctStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<RjctRow>, Box<dyn Error>> {
        RjctStream::next_row(self)
    }
}

impl RowStream for DbtRspnStream<Source> {
    type Row = DbtRspnRow;
    fn open(source: Source, name: &str) -> Self {
        DbtRspnStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<DbtRspnRow>, Box<dyn Error>> {
        DbtRspnStream::next_row(self)
    }
}

impl RowStream for DbtReqStream<Source> {
    type Row = DbtReqRow;
    fn open(source: Source, name: &str) -> Self {
        DbtReqStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<DbtReqRow>, Box<dyn Error>> {
        DbtReqStream::next_row(self)
    }
}

impl RowStream for ModfyStream<Source> {
    type Row = ModfyRow;
    fn open(source: Source, name: &str) -> Self {
        ModfyStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<ModfyRow>, Box<dyn Error>> {
        ModfyStream::next_row(self)
    }
}

impl RowStream for Mt101Stream<Source> {
    type Row = Mt101Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt101Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt101Row>, Box<dyn Error>> {
        Mt101Stream::next_row(self)
    }
}

impl RowStream for Mt104Stream<Source> {
    type Row = Mt104Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt104Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt104Row>, Box<dyn Error>> {
        Mt104Stream::next_row(self)
    }
}

impl RowStream for Mt103Stream<Source> {
    type Row = Mt103Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt103Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt103Row>, Box<dyn Error>> {
        Mt103Stream::next_row(self)
    }
}

impl RowStream for Mt202Stream<Source> {
    type Row = Mt202Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt202Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt202Row>, Box<dyn Error>> {
        Mt202Stream::next_row(self)
    }
}

impl RowStream for Mt940Stream<Source> {
    type Row = Mt940Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt940Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt940Row>, Box<dyn Error>> {
        Mt940Stream::next_row(self)
    }
}

impl RowStream for Mt942Stream<Source> {
    type Row = Mt942Row;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_container_only(input)
    }
    fn open(source: Source, name: &str) -> Self {
        Mt942Stream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<Mt942Row>, Box<dyn Error>> {
        Mt942Stream::next_row(self)
    }
}

impl RowStream for SniffStream<Source> {
    type Row = SniffRow;
    fn guard(_input: &mut Input) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn open(source: Source, name: &str) -> Self {
        SniffStream::new(source, name)
    }
    fn next_row(&mut self) -> Result<Option<SniffRow>, Box<dyn Error>> {
        SniffStream::next_row(self)
    }
}

// The address audit reads both wire formats, so it takes a guard of its own: the
// prefix decides which walk opens, and only a file that is neither is refused.
impl RowStream for Addresses<Source> {
    type Row = AddrRow;
    fn guard(input: &mut Input) -> Result<(), Box<dyn Error>> {
        guard_message_prefix(input)
    }
    fn open(source: Source, name: &str) -> Self {
        let mt = source.get_ref().shape == Some(Shape::Mt);
        Addresses::new(source, name, mt)
    }
    fn next_row(&mut self) -> Result<Option<AddrRow>, Box<dyn Error>> {
        Addresses::next_row(self)
    }
}

/// Where a scan is: which file, and its open reader.
struct ScanState<S> {
    idx: usize,
    cur: Option<S>,
}

impl<S> ScanState<S> {
    fn new() -> Self {
        ScanState { idx: 0, cur: None }
    }
}

/// Pull up to one vector of rows, advancing across files as each drains.
fn pull_batch<S: RowStream>(
    files: &[String],
    st: &mut ScanState<S>,
    fname: &str,
) -> Result<Vec<S::Row>, Box<dyn Error>> {
    let mut batch = Vec::with_capacity(VECTOR_SIZE);
    while batch.len() < VECTOR_SIZE {
        if st.cur.is_none() {
            if st.idx >= files.len() {
                break;
            }
            let path = files[st.idx].clone();
            let input = open_source(&path).map_err(|e| format!("{fname}: {e}"))?;
            st.cur = Some(S::from_input(input, &path).map_err(|e| format!("{fname}: {e}"))?);
        }
        match st.cur.as_mut().unwrap().next_row()? {
            Some(row) => batch.push(row),
            None => {
                st.cur = None;
                st.idx += 1;
            }
        }
    }
    Ok(batch)
}

// ── parallel scan ────────────────────────────────────────────────────────────

/// Where a multi-file scan is. Chosen on the first `func` call, because only
/// then are both the file count and the `threads` argument in hand.
enum Scan<S: RowStream> {
    Pending,
    Sequential(ScanState<S>),
    Parallel(mpsc::Receiver<std::result::Result<Vec<S::Row>, String>>),
}

/// How many worker threads a scan gets. An explicit `threads := n` wins, up to the file
/// count and four times the machine's parallelism (anything below 1 means sequential);
/// the default is one thread per file, capped at that parallelism. One file is always
/// sequential — XML has no safe split points, so a single document cannot be divided.
fn effective_threads(requested: Option<i64>, nfiles: usize) -> usize {
    let auto = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    match requested {
        // Four times the machine's parallelism is generous for work this
        // sequential; past that a thread costs a stack and buys nothing.
        Some(n) if n >= 1 => (n as usize).min(nfiles).min(auto * 4),
        Some(_) => 1,
        None => auto.min(nfiles),
    }
}

/// File-level parallelism. The unit of work is the whole file: workers claim
/// the next unparsed file from a shared counter, parse it into vector-sized
/// batches, and hand the batches over a bounded channel.
///
/// Bounded, so memory stays O(threads × batch) no matter how many files the
/// glob matched — the same discipline as the sequential scan, multiplied by
/// the worker count and the channel capacity. Rows of one file stay in file
/// order; files interleave nondeterministically, which is what `source_file`
/// is for. A `LIMIT` that stops the scan drops the receiver, every following
/// `send` fails, and the workers exit instead of parsing the rest of the glob.
///
/// Errors cross the channel as strings and abort the scan at the batch where
/// they surfaced, exactly as in the sequential path: a malformed amount in any
/// file still fails the whole query rather than dropping out of a `SUM`.
fn spawn_workers<S>(
    files: Vec<String>,
    threads: usize,
    fname: &'static str,
) -> mpsc::Receiver<std::result::Result<Vec<S::Row>, String>>
where
    S: RowStream + 'static,
    S::Row: Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(threads * 2);
    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    for _ in 0..threads {
        let tx = tx.clone();
        let files = Arc::clone(&files);
        let next = Arc::clone(&next);
        std::thread::spawn(move || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(path) = files.get(i) else { return };
            let input = match open_source(path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Err(format!("{fname}: {e}")));
                    return;
                }
            };
            let mut stream = match S::from_input(input, path) {
                Ok(stream) => stream,
                Err(e) => {
                    let _ = tx.send(Err(format!("{fname}: {e}")));
                    return;
                }
            };
            let mut batch = Vec::with_capacity(VECTOR_SIZE);
            loop {
                match stream.next_row() {
                    Ok(Some(row)) => {
                        batch.push(row);
                        if batch.len() == VECTOR_SIZE {
                            if tx.send(Ok(std::mem::take(&mut batch))).is_err() {
                                return; // scan stopped early (LIMIT, error)
                            }
                            batch.reserve(VECTOR_SIZE);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }
            if !batch.is_empty() && tx.send(Ok(batch)).is_err() {
                return;
            }
        });
    }
    // Drop the template sender: the channel disconnects when the last worker
    // finishes, which is how the scan knows it is done.
    drop(tx);
    rx
}

/// The next batch of rows, deciding sequential-vs-parallel on first call.
fn next_batch<S>(
    files: &[String],
    threads: Option<i64>,
    scan: &mut Scan<S>,
    fname: &'static str,
) -> std::result::Result<Vec<S::Row>, Box<dyn Error>>
where
    S: RowStream + 'static,
    S::Row: Send + 'static,
{
    if matches!(scan, Scan::Pending) {
        let t = effective_threads(threads, files.len());
        *scan = if t <= 1 {
            Scan::Sequential(ScanState::new())
        } else {
            Scan::Parallel(spawn_workers::<S>(files.to_vec(), t, fname))
        };
    }
    match scan {
        Scan::Sequential(st) => pull_batch(files, st, fname),
        Scan::Parallel(rx) => match rx.recv() {
            Ok(Ok(batch)) => Ok(batch),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(Vec::new()), // every worker done
        },
        Scan::Pending => unreachable!(),
    }
}

/// Expand a path or glob into a file list: local paths only, no directories, and
/// a literal name that glob refuses to compile still resolves.
fn resolve_files(pattern: &str, fname: &str) -> Result<Vec<String>, Box<dyn Error>> {
    if let Some(scheme) = remote_scheme(pattern) {
        return Err(format!(
            "{fname}: {scheme}:// paths are not supported; read a local file \
             (see docs/adr/0002-no-remote-paths.md)"
        )
        .into());
    }
    // A name a bank wrote is not a pattern anyone chose: `stmt[1.xml` is a
    // file, and glob refuses to compile it.
    let literal = std::path::Path::new(pattern);
    let mut files: Vec<String> = match glob::glob(pattern) {
        Ok(paths) => paths
            .filter_map(|p| p.ok())
            .filter(|p| openable(p))
            .map(|p| {
                p.display()
                    .to_string()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect(),
        Err(e) if !openable(literal) => {
            return Err(format!("bad path pattern {pattern:?}: {e}").into())
        }
        Err(_) => Vec::new(),
    };
    if files.is_empty() && openable(literal) {
        files.push(pattern.to_string());
    }
    if files.is_empty() {
        return Err(format!("{fname}: no files matched {pattern:?}").into());
    }
    Ok(files)
}

/// A path worth handing to the reader. Only directories are excluded: a glob
/// matches them and opening one is not a scan. Everything else that exists
/// stays, because `is_file` is false for a FIFO too, and a statement may
/// arrive down a pipe.
fn openable(p: &std::path::Path) -> bool {
    p.exists() && !p.is_dir()
}

/// The URI scheme of a path, when it has one. A Windows drive letter (`C:/…`) is
/// not a URI, hence the length check.
fn remote_scheme(path: &str) -> Option<&str> {
    let i = path.find("://")?;
    let scheme = &path[..i];
    (i > 1
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+'))
    .then_some(scheme)
}

// ── column declaration and writers ───────────────────────────────────────────

/// Column kinds this extension emits. Amounts are `DECIMAL`, never `DOUBLE`:
/// see `decimal`. Dates keep their wire precision: `TIMESTAMP` where the corpus
/// mixes date-only and date-time values, `DATE` where the schema says date.
#[derive(Clone, Copy)]
enum Col {
    Text,
    Date,
    Stamp,
    Money,
    Int,
}

impl Col {
    fn handle(self) -> LogicalTypeHandle {
        match self {
            Col::Text => LogicalTypeHandle::from(LogicalTypeId::Varchar),
            Col::Date => LogicalTypeHandle::from(LogicalTypeId::Date),
            Col::Stamp => LogicalTypeHandle::from(LogicalTypeId::Timestamp),
            Col::Money => LogicalTypeHandle::decimal(decimal::WIDTH, decimal::SCALE),
            Col::Int => LogicalTypeHandle::from(LogicalTypeId::Bigint),
        }
    }
}

fn declare(bind: &BindInfo, columns: &[(&str, Col)]) {
    for (name, col) in columns {
        bind.add_result_column(name, col.handle());
    }
}

/// A column index is written by hand in every `table_function!`, once per column,
/// and `flat_vector` does not check it: `duckdb_data_chunk_get_vector` past the
/// last column hands back whatever is at that offset, and writing through it
/// corrupts a neighbouring vector instead of failing. An off-by-one in a
/// forty-column reader then shows up as a garbage decimal three columns away.
/// The check costs one comparison per column per batch of 2048 rows.
fn column<'a>(output: &'a mut DataChunkHandle, idx: usize) -> FlatVector<'a> {
    in_range(idx, output.num_columns());
    output.flat_vector(idx)
}

/// Split out from [`column`] so a test can reach it: `DataChunkHandle` cannot be
/// constructed outside a loaded extension, because the C API function table is
/// installed by the entrypoint and a unit test has not run one.
#[track_caller]
fn in_range(idx: usize, columns: usize) {
    assert!(
        idx < columns,
        "column {idx} written on a chunk of {columns} columns"
    );
}

fn write_text<T>(
    output: &mut DataChunkHandle,
    idx: usize,
    batch: &[T],
    get: impl Fn(&T) -> &Option<String>,
) {
    let mut v = column(output, idx);
    for (i, row) in batch.iter().enumerate() {
        match get(row) {
            Some(s) => v.insert(i, s.as_str()),
            None => v.set_null(i),
        }
    }
}

/// Write a fixed-width numeric column. Values go through the raw slice in an
/// inner scope so the borrow ends before the vector is touched again for NULLs,
/// and the missing positions are recorded in a stack bitmap on the way past, so
/// each getter runs once per row rather than twice -- `Col::Stamp` parses a
/// timestamp string, and parsing every one of them twice is the whole cost of
/// the column. The bitmap is fixed size, which makes
/// `batch.len() <= VECTOR_SIZE` a precondition and not a hint.
macro_rules! write_numeric {
    ($name:ident, $ty:ty) => {
        fn $name<T>(
            output: &mut DataChunkHandle,
            idx: usize,
            batch: &[T],
            get: impl Fn(&T) -> Option<$ty>,
        ) {
            debug_assert!(batch.len() <= VECTOR_SIZE);
            let mut v = column(output, idx);
            let mut nulls = [0u64; VECTOR_SIZE / 64];
            {
                let slice = unsafe { v.as_mut_slice::<$ty>() };
                for (i, row) in batch.iter().enumerate() {
                    match get(row) {
                        Some(x) => slice[i] = x,
                        None => nulls[i / 64] |= 1 << (i % 64),
                    }
                }
            }
            for i in 0..batch.len() {
                if nulls[i / 64] >> (i % 64) & 1 == 1 {
                    v.set_null(i);
                }
            }
        }
    };
}

write_numeric!(write_date, i32);
write_numeric!(write_timestamp, i64);
// DECIMAL(38,5) is physically INT128.
write_numeric!(write_decimal, i128);
write_numeric!(write_bigint, i64);

/// Files resolved at bind time, plus the requested worker count. Shared by
/// every table function.
#[repr(C)]
struct FileList {
    files: Vec<String>,
    threads: Option<i64>,
}

/// Generates the boilerplate every table function repeats: bind resolves the
/// file list and declares columns, init opens a scan, `parameters` takes one
/// path. Only the column writing differs, so only that is spelled out.
macro_rules! table_function {
    (
        $vtab:ident, $init:ident, $stream:ty, $row:ty,
        name = $sql_name:literal,
        columns = $columns:expr,
        write = |$output:ident, $batch:ident| $write:block
    ) => {
        #[repr(C)]
        struct $init {
            state: Mutex<Scan<$stream>>,
        }

        struct $vtab;

        impl VTab for $vtab {
            type InitData = $init;
            type BindData = FileList;

            fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
                declare(bind, $columns);
                Ok(FileList {
                    files: resolve_files(&bind.get_parameter(0).to_string(), $sql_name)?,
                    threads: bind.get_named_parameter("threads").map(|v| v.to_int64()),
                })
            }

            fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
                Ok($init {
                    state: Mutex::new(Scan::Pending),
                })
            }

            fn func(
                func: &TableFunctionInfo<Self>,
                $output: &mut DataChunkHandle,
            ) -> Result<(), Box<dyn Error>> {
                let bind_data = func.get_bind_data();
                let mut st = func.get_init_data().state.lock();
                let $batch: Vec<$row> =
                    next_batch(&bind_data.files, bind_data.threads, &mut st, $sql_name)?;
                // The lock only guards the scan cursor, not the writing below.
                drop(st);
                $write
                $output.set_len($batch.len());
                Ok(())
            }

            fn parameters() -> Option<Vec<LogicalTypeHandle>> {
                Some(vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)])
            }

            fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
                Some(vec![(
                    "threads".to_string(),
                    LogicalTypeHandle::from(LogicalTypeId::Bigint),
                )])
            }
        }
    };
}

// ── read_iso20022: camt.053 / camt.054 / camt.052 ────────────────────────────

const CAMT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("account_iban", Col::Text),
    ("statement_id", Col::Text),
    ("entry_ref", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("credit_debit", Col::Text),
    ("status", Col::Text),
    ("booking_date", Col::Stamp),
    ("value_date", Col::Stamp),
    ("bank_ref", Col::Text),
    ("end_to_end_id", Col::Text),
    ("counterparty_name", Col::Text),
    ("counterparty_iban", Col::Text),
    ("remittance_info", Col::Text),
    // The grain the supplementary readers join on. NULL together, for an entry
    // that is not a direct child of a statement: ADR 0004 keeps such an entry
    // as a row, and an index for it would be a key pointing at nothing.
    ("statement_kind", Col::Text),
    ("statement_index", Col::Int),
    ("entry_index", Col::Int),
    // What is under the entry, so the four columns above that are NULL on a
    // batch say why rather than just being empty.
    ("transaction_count", Col::Int),
    ("remittance_count", Col::Int),
    ("reversal_indicator", Col::Text),
    ("bank_transaction_domain", Col::Text),
    ("bank_transaction_family", Col::Text),
    ("bank_transaction_subfamily", Col::Text),
    ("bank_transaction_proprietary", Col::Text),
    ("bank_transaction_proprietary_issuer", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadIso20022, CamtInit, EntryStream<Source>, Row,
    name = "read_iso20022",
    columns = CAMT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 4, &batch, |r: &Row| r.amount);
        write_timestamp(output, 8, &batch, |r: &Row| {
            r.booking_date.as_deref().and_then(temporal::ts_micros)
        });
        write_timestamp(output, 9, &batch, |r: &Row| {
            r.value_date.as_deref().and_then(temporal::ts_micros)
        });
        write_bigint(output, 16, &batch, |r: &Row| r.statement_index);
        write_bigint(output, 17, &batch, |r: &Row| r.entry_index);
        write_bigint(output, 18, &batch, |r: &Row| Some(r.transaction_count));
        write_bigint(output, 19, &batch, |r: &Row| Some(r.remittance_count));
        write_text(output, 0, &batch, |r: &Row| &r.msg_id);
        write_text(output, 1, &batch, |r: &Row| &r.account_iban);
        write_text(output, 2, &batch, |r: &Row| &r.statement_id);
        write_text(output, 3, &batch, |r: &Row| &r.entry_ref);
        write_text(output, 5, &batch, |r: &Row| &r.currency);
        write_text(output, 6, &batch, |r: &Row| &r.credit_debit);
        write_text(output, 7, &batch, |r: &Row| &r.status);
        write_text(output, 10, &batch, |r: &Row| &r.bank_ref);
        write_text(output, 11, &batch, |r: &Row| &r.end_to_end_id);
        write_text(output, 12, &batch, |r: &Row| &r.counterparty_name);
        write_text(output, 13, &batch, |r: &Row| &r.counterparty_iban);
        write_text(output, 14, &batch, |r: &Row| &r.remittance_info);
        write_text(output, 15, &batch, |r: &Row| &r.statement_kind);
        write_text(output, 20, &batch, |r: &Row| &r.reversal_indicator);
        write_text(output, 21, &batch, |r: &Row| &r.bank_transaction_domain);
        write_text(output, 22, &batch, |r: &Row| &r.bank_transaction_family);
        write_text(output, 23, &batch, |r: &Row| &r.bank_transaction_subfamily);
        write_text(output, 24, &batch, |r: &Row| &r.bank_transaction_proprietary);
        write_text(output, 25, &batch, |r: &Row| {
            &r.bank_transaction_proprietary_issuer
        });
        write_text(output, 26, &batch, |r: &Row| &r.source_file);
    }
}

// ── read_camt_transactions ───────────────────────────────────────────────────

const CAMT_TX_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("statement_kind", Col::Text),
    ("statement_index", Col::Int),
    ("statement_id", Col::Text),
    ("account_iban", Col::Text),
    ("account_currency", Col::Text),
    ("entry_index", Col::Int),
    ("entry_ref", Col::Text),
    ("entry_amount", Col::Money),
    ("entry_currency", Col::Text),
    ("entry_credit_debit", Col::Text),
    ("entry_reversal_indicator", Col::Text),
    ("entry_status", Col::Text),
    ("booking_date", Col::Stamp),
    ("value_date", Col::Stamp),
    ("bank_ref", Col::Text),
    ("entry_bank_transaction_domain", Col::Text),
    ("entry_bank_transaction_family", Col::Text),
    ("entry_bank_transaction_subfamily", Col::Text),
    ("entry_bank_transaction_proprietary", Col::Text),
    ("entry_bank_transaction_proprietary_issuer", Col::Text),
    ("entry_details_index", Col::Int),
    ("transaction_index", Col::Int),
    ("batch_message_id", Col::Text),
    ("batch_payment_info_id", Col::Text),
    // A wire count, kept as spelled: what the sender said the batch held is
    // not always how many transactions are here, and a BIGINT would round a
    // disagreement into a number.
    ("batch_number_of_transactions", Col::Text),
    ("batch_total_amount", Col::Money),
    ("batch_total_currency", Col::Text),
    ("batch_credit_debit", Col::Text),
    ("instruction_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("transaction_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("credit_debit", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("ultimate_debtor_name", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("ultimate_creditor_name", Col::Text),
    ("bank_transaction_domain", Col::Text),
    ("bank_transaction_family", Col::Text),
    ("bank_transaction_subfamily", Col::Text),
    ("bank_transaction_proprietary", Col::Text),
    ("bank_transaction_proprietary_issuer", Col::Text),
    ("remittance_count", Col::Int),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamtTransactions, CamtTxInit, TransactionStream<Source>, TransactionRow,
    name = "read_camt_transactions",
    columns = CAMT_TX_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 8, &batch, |r: &TransactionRow| r.entry_amount);
        write_timestamp(output, 13, &batch, |r: &TransactionRow| {
            r.booking_date.as_deref().and_then(temporal::ts_micros)
        });
        write_timestamp(output, 14, &batch, |r: &TransactionRow| {
            r.value_date.as_deref().and_then(temporal::ts_micros)
        });
        write_decimal(output, 26, &batch, |r: &TransactionRow| r.batch_total_amount);
        write_decimal(output, 33, &batch, |r: &TransactionRow| r.amount);
        write_bigint(output, 2, &batch, |r: &TransactionRow| r.statement_index);
        write_bigint(output, 6, &batch, |r: &TransactionRow| r.entry_index);
        write_bigint(output, 21, &batch, |r: &TransactionRow| r.entry_details_index);
        write_bigint(output, 22, &batch, |r: &TransactionRow| r.transaction_index);
        write_bigint(output, 47, &batch, |r: &TransactionRow| r.remittance_count);
        write_text(output, 0, &batch, |r: &TransactionRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &TransactionRow| &r.statement_kind);
        write_text(output, 3, &batch, |r: &TransactionRow| &r.statement_id);
        write_text(output, 4, &batch, |r: &TransactionRow| &r.account_iban);
        write_text(output, 5, &batch, |r: &TransactionRow| &r.account_currency);
        write_text(output, 7, &batch, |r: &TransactionRow| &r.entry_ref);
        write_text(output, 9, &batch, |r: &TransactionRow| &r.entry_currency);
        write_text(output, 10, &batch, |r: &TransactionRow| &r.entry_credit_debit);
        write_text(output, 11, &batch, |r: &TransactionRow| {
            &r.entry_reversal_indicator
        });
        write_text(output, 12, &batch, |r: &TransactionRow| &r.entry_status);
        write_text(output, 15, &batch, |r: &TransactionRow| &r.bank_ref);
        write_text(output, 16, &batch, |r: &TransactionRow| {
            &r.entry_bank_transaction_domain
        });
        write_text(output, 17, &batch, |r: &TransactionRow| {
            &r.entry_bank_transaction_family
        });
        write_text(output, 18, &batch, |r: &TransactionRow| {
            &r.entry_bank_transaction_subfamily
        });
        write_text(output, 19, &batch, |r: &TransactionRow| {
            &r.entry_bank_transaction_proprietary
        });
        write_text(output, 20, &batch, |r: &TransactionRow| {
            &r.entry_bank_transaction_proprietary_issuer
        });
        write_text(output, 23, &batch, |r: &TransactionRow| &r.batch_message_id);
        write_text(output, 24, &batch, |r: &TransactionRow| {
            &r.batch_payment_info_id
        });
        write_text(output, 25, &batch, |r: &TransactionRow| {
            &r.batch_number_of_transactions
        });
        write_text(output, 27, &batch, |r: &TransactionRow| &r.batch_total_currency);
        write_text(output, 28, &batch, |r: &TransactionRow| &r.batch_credit_debit);
        write_text(output, 29, &batch, |r: &TransactionRow| &r.instruction_id);
        write_text(output, 30, &batch, |r: &TransactionRow| &r.end_to_end_id);
        write_text(output, 31, &batch, |r: &TransactionRow| &r.transaction_id);
        write_text(output, 32, &batch, |r: &TransactionRow| &r.uetr);
        write_text(output, 34, &batch, |r: &TransactionRow| &r.currency);
        write_text(output, 35, &batch, |r: &TransactionRow| &r.credit_debit);
        write_text(output, 36, &batch, |r: &TransactionRow| &r.debtor_name);
        write_text(output, 37, &batch, |r: &TransactionRow| &r.debtor_account);
        write_text(output, 38, &batch, |r: &TransactionRow| {
            &r.ultimate_debtor_name
        });
        write_text(output, 39, &batch, |r: &TransactionRow| &r.creditor_name);
        write_text(output, 40, &batch, |r: &TransactionRow| &r.creditor_account);
        write_text(output, 41, &batch, |r: &TransactionRow| {
            &r.ultimate_creditor_name
        });
        write_text(output, 42, &batch, |r: &TransactionRow| {
            &r.bank_transaction_domain
        });
        write_text(output, 43, &batch, |r: &TransactionRow| {
            &r.bank_transaction_family
        });
        write_text(output, 44, &batch, |r: &TransactionRow| {
            &r.bank_transaction_subfamily
        });
        write_text(output, 45, &batch, |r: &TransactionRow| {
            &r.bank_transaction_proprietary
        });
        write_text(output, 46, &batch, |r: &TransactionRow| {
            &r.bank_transaction_proprietary_issuer
        });
        write_text(output, 48, &batch, |r: &TransactionRow| &r.source_file);
    }
}

// ── read_camt_balances ───────────────────────────────────────────────────────

const CAMT_BAL_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("statement_kind", Col::Text),
    ("statement_index", Col::Int),
    ("statement_id", Col::Text),
    ("account_iban", Col::Text),
    ("account_currency", Col::Text),
    ("balance_index", Col::Int),
    ("balance_type", Col::Text),
    // Which vocabulary the value came from: `OPBD` and a bank's own
    // `INTRADAY-PEAK` are not the same kind of fact, and one column could not
    // say so.
    ("balance_type_scheme", Col::Text),
    ("balance_subtype", Col::Text),
    ("balance_subtype_scheme", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("credit_debit", Col::Text),
    ("balance_date", Col::Stamp),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamtBalances, CamtBalInit, BalanceStream<Source>, BalanceRow,
    name = "read_camt_balances",
    columns = CAMT_BAL_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 11, &batch, |r: &BalanceRow| r.amount);
        write_timestamp(output, 14, &batch, |r: &BalanceRow| {
            r.balance_date.as_deref().and_then(temporal::ts_micros)
        });
        write_bigint(output, 2, &batch, |r: &BalanceRow| r.statement_index);
        write_bigint(output, 6, &batch, |r: &BalanceRow| r.balance_index);
        write_text(output, 0, &batch, |r: &BalanceRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &BalanceRow| &r.statement_kind);
        write_text(output, 3, &batch, |r: &BalanceRow| &r.statement_id);
        write_text(output, 4, &batch, |r: &BalanceRow| &r.account_iban);
        write_text(output, 5, &batch, |r: &BalanceRow| &r.account_currency);
        write_text(output, 7, &batch, |r: &BalanceRow| &r.balance_type);
        write_text(output, 8, &batch, |r: &BalanceRow| &r.balance_type_scheme);
        write_text(output, 9, &batch, |r: &BalanceRow| &r.balance_subtype);
        write_text(output, 10, &batch, |r: &BalanceRow| &r.balance_subtype_scheme);
        write_text(output, 12, &batch, |r: &BalanceRow| &r.currency);
        write_text(output, 13, &batch, |r: &BalanceRow| &r.credit_debit);
        write_text(output, 15, &batch, |r: &BalanceRow| &r.source_file);
    }
}

// ── read_camt_amount_details ─────────────────────────────────────────────────

const CAMT_AMT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("statement_kind", Col::Text),
    ("statement_index", Col::Int),
    ("statement_id", Col::Text),
    ("account_iban", Col::Text),
    ("entry_index", Col::Int),
    ("entry_ref", Col::Text),
    // NULL on an entry-level block, populated on a transaction-level one.
    ("entry_details_index", Col::Int),
    ("transaction_index", Col::Int),
    ("scope", Col::Text),
    ("amount_kind", Col::Text),
    ("amount_index", Col::Int),
    ("proprietary_type", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("exchange_source_currency", Col::Text),
    ("exchange_target_currency", Col::Text),
    ("exchange_unit_currency", Col::Text),
    // A rate is not money: it keeps the lexical value the wire carried, because
    // the five fraction digits an ISO 20022 amount allows would round a
    // ten-digit rate or refuse the file over it.
    ("exchange_rate", Col::Text),
    ("exchange_contract_id", Col::Text),
    ("exchange_quotation_time", Col::Stamp),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamtAmountDetails, CamtAmtInit, AmountDetailStream<Source>, AmountDetailRow,
    name = "read_camt_amount_details",
    columns = CAMT_AMT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 13, &batch, |r: &AmountDetailRow| r.amount);
        write_timestamp(output, 20, &batch, |r: &AmountDetailRow| {
            r.exchange_quotation_time
                .as_deref()
                .and_then(temporal::ts_micros)
        });
        write_bigint(output, 2, &batch, |r: &AmountDetailRow| r.statement_index);
        write_bigint(output, 5, &batch, |r: &AmountDetailRow| r.entry_index);
        write_bigint(output, 7, &batch, |r: &AmountDetailRow| {
            r.entry_details_index
        });
        write_bigint(output, 8, &batch, |r: &AmountDetailRow| r.transaction_index);
        write_bigint(output, 11, &batch, |r: &AmountDetailRow| r.amount_index);
        write_text(output, 0, &batch, |r: &AmountDetailRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &AmountDetailRow| &r.statement_kind);
        write_text(output, 3, &batch, |r: &AmountDetailRow| &r.statement_id);
        write_text(output, 4, &batch, |r: &AmountDetailRow| &r.account_iban);
        write_text(output, 6, &batch, |r: &AmountDetailRow| &r.entry_ref);
        write_text(output, 9, &batch, |r: &AmountDetailRow| &r.scope);
        write_text(output, 10, &batch, |r: &AmountDetailRow| &r.amount_kind);
        write_text(output, 12, &batch, |r: &AmountDetailRow| &r.proprietary_type);
        write_text(output, 14, &batch, |r: &AmountDetailRow| &r.currency);
        write_text(output, 15, &batch, |r: &AmountDetailRow| {
            &r.exchange_source_currency
        });
        write_text(output, 16, &batch, |r: &AmountDetailRow| {
            &r.exchange_target_currency
        });
        write_text(output, 17, &batch, |r: &AmountDetailRow| {
            &r.exchange_unit_currency
        });
        write_text(output, 18, &batch, |r: &AmountDetailRow| &r.exchange_rate);
        write_text(output, 19, &batch, |r: &AmountDetailRow| {
            &r.exchange_contract_id
        });
        write_text(output, 21, &batch, |r: &AmountDetailRow| &r.source_file);
    }
}

// ── read_camt_remittance ─────────────────────────────────────────────────────

const CAMT_RMT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("statement_kind", Col::Text),
    ("statement_index", Col::Int),
    ("statement_id", Col::Text),
    ("account_iban", Col::Text),
    ("entry_index", Col::Int),
    ("entry_ref", Col::Text),
    ("entry_details_index", Col::Int),
    ("transaction_index", Col::Int),
    ("remittance_index", Col::Int),
    // The owning `<Strd>` ordinal, NULL for a `<Ustrd>`. It counts earlier
    // blocks that emitted no supported leaf, so it is a position in the message
    // and not a position in the output.
    ("structured_index", Col::Int),
    ("slot", Col::Text),
    ("text", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamtRemittance, CamtRmtInit, RemittanceStream<Source>, RemittanceRow,
    name = "read_camt_remittance",
    columns = CAMT_RMT_COLUMNS,
    write = |output, batch| {
        write_bigint(output, 2, &batch, |r: &RemittanceRow| r.statement_index);
        write_bigint(output, 5, &batch, |r: &RemittanceRow| r.entry_index);
        write_bigint(output, 7, &batch, |r: &RemittanceRow| r.entry_details_index);
        write_bigint(output, 8, &batch, |r: &RemittanceRow| r.transaction_index);
        write_bigint(output, 9, &batch, |r: &RemittanceRow| r.remittance_index);
        write_bigint(output, 10, &batch, |r: &RemittanceRow| r.structured_index);
        write_text(output, 0, &batch, |r: &RemittanceRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RemittanceRow| &r.statement_kind);
        write_text(output, 3, &batch, |r: &RemittanceRow| &r.statement_id);
        write_text(output, 4, &batch, |r: &RemittanceRow| &r.account_iban);
        write_text(output, 6, &batch, |r: &RemittanceRow| &r.entry_ref);
        write_text(output, 11, &batch, |r: &RemittanceRow| &r.slot);
        write_text(output, 12, &batch, |r: &RemittanceRow| &r.text);
        write_text(output, 13, &batch, |r: &RemittanceRow| &r.source_file);
    }
}

// ── read_pacs008 ─────────────────────────────────────────────────────────────

const PACS_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("tx_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("settlement_date", Col::Date),
    ("charge_bearer", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs008, PacsInit, TxStream<Source>, PacsRow,
    name = "read_pacs008",
    columns = PACS_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 5, &batch, |r: &PacsRow| r.amount);
        write_date(output, 7, &batch, |r: &PacsRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &PacsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &PacsRow| &r.instr_id);
        write_text(output, 2, &batch, |r: &PacsRow| &r.end_to_end_id);
        write_text(output, 3, &batch, |r: &PacsRow| &r.tx_id);
        write_text(output, 4, &batch, |r: &PacsRow| &r.uetr);
        write_text(output, 6, &batch, |r: &PacsRow| &r.currency);
        write_text(output, 8, &batch, |r: &PacsRow| &r.charge_bearer);
        write_text(output, 9, &batch, |r: &PacsRow| &r.debtor_name);
        write_text(output, 10, &batch, |r: &PacsRow| &r.debtor_account);
        write_text(output, 11, &batch, |r: &PacsRow| &r.debtor_agent_bic);
        write_text(output, 12, &batch, |r: &PacsRow| &r.creditor_name);
        write_text(output, 13, &batch, |r: &PacsRow| &r.creditor_account);
        write_text(output, 14, &batch, |r: &PacsRow| &r.creditor_agent_bic);
        write_text(output, 15, &batch, |r: &PacsRow| &r.remittance_info);
        write_text(output, 16, &batch, |r: &PacsRow| &r.source_file);
    }
}

// ── read_pain001 ─────────────────────────────────────────────────────────────

const PAIN_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("payment_info_id", Col::Text),
    ("payment_method", Col::Text),
    ("requested_execution_date", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    // The tracking reference that follows one payment across message families:
    // the same UETR appears on the pacs.008 that settles it and on the pacs.004
    // that returns it, which is what makes those readers joinable.
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("charge_bearer", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain001, PainInit, PainStream<Source>, PainRow,
    name = "read_pain001",
    columns = PAIN_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 11, &batch, |r: &PainRow| r.amount);
        write_date(output, 4, &batch, |r: &PainRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &PainRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &PainRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &PainRow| &r.payment_info_id);
        write_text(output, 3, &batch, |r: &PainRow| &r.payment_method);
        write_text(output, 5, &batch, |r: &PainRow| &r.debtor_name);
        write_text(output, 6, &batch, |r: &PainRow| &r.debtor_account);
        write_text(output, 7, &batch, |r: &PainRow| &r.debtor_agent_bic);
        write_text(output, 8, &batch, |r: &PainRow| &r.instr_id);
        write_text(output, 9, &batch, |r: &PainRow| &r.end_to_end_id);
        write_text(output, 10, &batch, |r: &PainRow| &r.uetr);
        write_text(output, 12, &batch, |r: &PainRow| &r.currency);
        write_text(output, 13, &batch, |r: &PainRow| &r.charge_bearer);
        write_text(output, 14, &batch, |r: &PainRow| &r.creditor_name);
        write_text(output, 15, &batch, |r: &PainRow| &r.creditor_account);
        write_text(output, 16, &batch, |r: &PainRow| &r.creditor_agent_bic);
        write_text(output, 17, &batch, |r: &PainRow| &r.remittance_info);
        write_text(output, 18, &batch, |r: &PainRow| &r.source_file);
    }
}

// ── read_pacs004 ─────────────────────────────────────────────────────────────

const RTR_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("return_id", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // What came back, and what the payment had settled for. Equal on a full
    // return; `amount < original_amount` is a return with charges deducted.
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("settlement_date", Col::Date),
    ("original_settlement_date", Col::Date),
    ("charge_bearer", Col::Text),
    ("return_reason_code", Col::Text),
    ("return_reason_info", Col::Text),
    ("return_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_debtor_account", Col::Text),
    ("original_debtor_agent_bic", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("original_creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs004, RtrInit, RtrStream<Source>, RtrRow,
    name = "read_pacs004",
    columns = RTR_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 8, &batch, |r: &RtrRow| r.amount);
        write_decimal(output, 10, &batch, |r: &RtrRow| r.original_amount);
        write_date(output, 12, &batch, |r: &RtrRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_date(output, 13, &batch, |r: &RtrRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RtrRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RtrRow| &r.return_id);
        write_text(output, 2, &batch, |r: &RtrRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &RtrRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &RtrRow| &r.original_instr_id);
        write_text(output, 5, &batch, |r: &RtrRow| &r.original_end_to_end_id);
        write_text(output, 6, &batch, |r: &RtrRow| &r.original_tx_id);
        write_text(output, 7, &batch, |r: &RtrRow| &r.original_uetr);
        write_text(output, 9, &batch, |r: &RtrRow| &r.currency);
        write_text(output, 11, &batch, |r: &RtrRow| &r.original_currency);
        write_text(output, 14, &batch, |r: &RtrRow| &r.charge_bearer);
        write_text(output, 15, &batch, |r: &RtrRow| &r.return_reason_code);
        write_text(output, 16, &batch, |r: &RtrRow| &r.return_reason_info);
        write_text(output, 17, &batch, |r: &RtrRow| &r.return_originator);
        write_text(output, 18, &batch, |r: &RtrRow| &r.original_debtor_name);
        write_text(output, 19, &batch, |r: &RtrRow| &r.original_debtor_account);
        write_text(output, 20, &batch, |r: &RtrRow| &r.original_debtor_agent_bic);
        write_text(output, 21, &batch, |r: &RtrRow| &r.original_creditor_name);
        write_text(output, 22, &batch, |r: &RtrRow| &r.original_creditor_account);
        write_text(output, 23, &batch, |r: &RtrRow| &r.original_creditor_agent_bic);
        write_text(output, 24, &batch, |r: &RtrRow| &r.remittance_info);
        write_text(output, 25, &batch, |r: &RtrRow| &r.source_file);
    }
}

// ── read_pain002 ─────────────────────────────────────────────────────────────

const STS_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    // Which level of the report this row states: GROUP, PAYMENT_INFO or
    // TRANSACTION. Only TRANSACTION rows carry an amount.
    ("status_level", Col::Text),
    ("original_payment_info_id", Col::Text),
    ("status_id", Col::Text),
    ("status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    // A count, not an amount: kept as the wire spelled it.
    ("original_number_of_txs", Col::Text),
    ("original_control_sum", Col::Money),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("requested_execution_date", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("acceptance_date_time", Col::Stamp),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain002, StsInit, StsStream<Source>, StsRow,
    name = "read_pain002",
    columns = STS_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 12, &batch, |r: &StsRow| r.original_control_sum);
        write_decimal(output, 16, &batch, |r: &StsRow| r.amount);
        write_date(output, 18, &batch, |r: &StsRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_timestamp(output, 24, &batch, |r: &StsRow| {
            r.acceptance_date_time.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &StsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &StsRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &StsRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &StsRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &StsRow| &r.status_level);
        write_text(output, 5, &batch, |r: &StsRow| &r.original_payment_info_id);
        write_text(output, 6, &batch, |r: &StsRow| &r.status_id);
        write_text(output, 7, &batch, |r: &StsRow| &r.status);
        write_text(output, 8, &batch, |r: &StsRow| &r.reason_code);
        write_text(output, 9, &batch, |r: &StsRow| &r.reason_info);
        write_text(output, 10, &batch, |r: &StsRow| &r.reason_originator);
        write_text(output, 11, &batch, |r: &StsRow| &r.original_number_of_txs);
        write_text(output, 13, &batch, |r: &StsRow| &r.original_instr_id);
        write_text(output, 14, &batch, |r: &StsRow| &r.original_end_to_end_id);
        write_text(output, 15, &batch, |r: &StsRow| &r.original_uetr);
        write_text(output, 17, &batch, |r: &StsRow| &r.currency);
        write_text(output, 19, &batch, |r: &StsRow| &r.debtor_name);
        write_text(output, 20, &batch, |r: &StsRow| &r.debtor_account);
        write_text(output, 21, &batch, |r: &StsRow| &r.creditor_name);
        write_text(output, 22, &batch, |r: &StsRow| &r.creditor_account);
        write_text(output, 23, &batch, |r: &StsRow| &r.remittance_info);
        write_text(output, 25, &batch, |r: &StsRow| &r.source_file);
    }
}

// ── read_pacs002 ─────────────────────────────────────────────────────────────

const RPT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    // Who is reporting to whom; per-transaction agents override the group pair.
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    // GROUP or TRANSACTION; the group block is optional in pacs.002, so a file
    // may contain only transaction rows.
    ("status_level", Col::Text),
    ("status_id", Col::Text),
    ("status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    ("acceptance_date_time", Col::Stamp),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs002, RptInit, RptStream<Source>, RptRow,
    name = "read_pacs002",
    columns = RPT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 16, &batch, |r: &RptRow| r.original_amount);
        write_timestamp(output, 15, &batch, |r: &RptRow| {
            r.acceptance_date_time.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 18, &batch, |r: &RptRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RptRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RptRow| &r.instructing_agent_bic);
        write_text(output, 2, &batch, |r: &RptRow| &r.instructed_agent_bic);
        write_text(output, 3, &batch, |r: &RptRow| &r.status_level);
        write_text(output, 4, &batch, |r: &RptRow| &r.status_id);
        write_text(output, 5, &batch, |r: &RptRow| &r.status);
        write_text(output, 6, &batch, |r: &RptRow| &r.reason_code);
        write_text(output, 7, &batch, |r: &RptRow| &r.reason_info);
        write_text(output, 8, &batch, |r: &RptRow| &r.reason_originator);
        write_text(output, 9, &batch, |r: &RptRow| &r.original_msg_id);
        write_text(output, 10, &batch, |r: &RptRow| &r.original_msg_name_id);
        write_text(output, 11, &batch, |r: &RptRow| &r.original_instr_id);
        write_text(output, 12, &batch, |r: &RptRow| &r.original_end_to_end_id);
        write_text(output, 13, &batch, |r: &RptRow| &r.original_tx_id);
        write_text(output, 14, &batch, |r: &RptRow| &r.original_uetr);
        write_text(output, 17, &batch, |r: &RptRow| &r.original_currency);
        write_text(output, 19, &batch, |r: &RptRow| &r.original_debtor_name);
        write_text(output, 20, &batch, |r: &RptRow| &r.original_creditor_name);
        write_text(output, 21, &batch, |r: &RptRow| &r.source_file);
    }
}

// ── read_pacs028 ─────────────────────────────────────────────────────────────

const STSREQ_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    // Who is asking whom; group-header pair, carried to every row.
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    // GROUP (status of a whole original message, no transaction detail) or
    // TRANSACTION. A request carries no status of its own, so this names the
    // grain, as `scope` does in read_camt056.
    ("scope", Col::Text),
    ("status_request_id", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // A request moves no money: there is no `amount`, only the original's,
    // from the carried copy when the request includes one.
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs028, StsReqInit, StsReqStream<Source>, StsReqRow,
    name = "read_pacs028",
    columns = STSREQ_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 11, &batch, |r: &StsReqRow| r.original_amount);
        write_date(output, 13, &batch, |r: &StsReqRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &StsReqRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &StsReqRow| &r.instructing_agent_bic);
        write_text(output, 2, &batch, |r: &StsReqRow| &r.instructed_agent_bic);
        write_text(output, 3, &batch, |r: &StsReqRow| &r.scope);
        write_text(output, 4, &batch, |r: &StsReqRow| &r.status_request_id);
        write_text(output, 5, &batch, |r: &StsReqRow| &r.original_msg_id);
        write_text(output, 6, &batch, |r: &StsReqRow| &r.original_msg_name_id);
        write_text(output, 7, &batch, |r: &StsReqRow| &r.original_instr_id);
        write_text(output, 8, &batch, |r: &StsReqRow| &r.original_end_to_end_id);
        write_text(output, 9, &batch, |r: &StsReqRow| &r.original_tx_id);
        write_text(output, 10, &batch, |r: &StsReqRow| &r.original_uetr);
        write_text(output, 12, &batch, |r: &StsReqRow| &r.original_currency);
        write_text(output, 14, &batch, |r: &StsReqRow| &r.original_debtor_name);
        write_text(output, 15, &batch, |r: &StsReqRow| &r.original_creditor_name);
        write_text(output, 16, &batch, |r: &StsReqRow| &r.source_file);
    }
}

// ── read_camt056 ─────────────────────────────────────────────────────────────

const CXL_COLUMNS: &[(&str, Col)] = &[
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    // GROUP (a whole underlying batch, possibly GrpCxl) or TRANSACTION.
    ("scope", Col::Text),
    ("cancellation_id", Col::Text),
    ("case_id", Col::Text),
    // As the wire spelled it; "true" means the whole batch is to be cancelled.
    ("group_cancellation", Col::Text),
    ("original_number_of_txs", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // A cancellation moves no money: there is no `amount`, only the original's.
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("cancellation_reason_code", Col::Text),
    ("cancellation_reason_info", Col::Text),
    ("cancellation_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_debtor_account", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt056, CxlInit, CxlStream<Source>, CxlRow,
    name = "read_camt056",
    columns = CXL_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 15, &batch, |r: &CxlRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &CxlRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 17, &batch, |r: &CxlRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &CxlRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &CxlRow| &r.assigner);
        write_text(output, 3, &batch, |r: &CxlRow| &r.assignee);
        write_text(output, 4, &batch, |r: &CxlRow| &r.scope);
        write_text(output, 5, &batch, |r: &CxlRow| &r.cancellation_id);
        write_text(output, 6, &batch, |r: &CxlRow| &r.case_id);
        write_text(output, 7, &batch, |r: &CxlRow| &r.group_cancellation);
        write_text(output, 8, &batch, |r: &CxlRow| &r.original_number_of_txs);
        write_text(output, 9, &batch, |r: &CxlRow| &r.original_msg_id);
        write_text(output, 10, &batch, |r: &CxlRow| &r.original_msg_name_id);
        write_text(output, 11, &batch, |r: &CxlRow| &r.original_instr_id);
        write_text(output, 12, &batch, |r: &CxlRow| &r.original_end_to_end_id);
        write_text(output, 13, &batch, |r: &CxlRow| &r.original_tx_id);
        write_text(output, 14, &batch, |r: &CxlRow| &r.original_uetr);
        write_text(output, 16, &batch, |r: &CxlRow| &r.original_currency);
        write_text(output, 18, &batch, |r: &CxlRow| &r.cancellation_reason_code);
        write_text(output, 19, &batch, |r: &CxlRow| &r.cancellation_reason_info);
        write_text(output, 20, &batch, |r: &CxlRow| &r.cancellation_originator);
        write_text(output, 21, &batch, |r: &CxlRow| &r.original_debtor_name);
        write_text(output, 22, &batch, |r: &CxlRow| &r.original_debtor_account);
        write_text(output, 23, &batch, |r: &CxlRow| &r.original_creditor_name);
        write_text(output, 24, &batch, |r: &CxlRow| &r.original_creditor_account);
        write_text(output, 25, &batch, |r: &CxlRow| &r.remittance_info);
        write_text(output, 26, &batch, |r: &CxlRow| &r.source_file);
    }
}

// ── read_camt057 ─────────────────────────────────────────────────────────────

const NTFCTN_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    // One notification per account; its items are the grain.
    ("notification_id", Col::Text),
    ("account", Col::Text),
    ("item_id", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    // A date-or-date-time choice, and the time is the point: an expected
    // credit at 14:00 is an intraday funding fact, so this keeps the
    // precision the wire carried rather than truncating to a day.
    ("expected_value_date", Col::Stamp),
    ("debtor_name", Col::Text),
    ("purpose", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt057, NtfctnInit, NtfctnStream<Source>, NtfctnRow,
    name = "read_camt057",
    columns = NTFCTN_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 4, &batch, |r: &NtfctnRow| r.amount);
        write_timestamp(output, 6, &batch, |r: &NtfctnRow| {
            r.expected_value_date.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &NtfctnRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &NtfctnRow| &r.notification_id);
        write_text(output, 2, &batch, |r: &NtfctnRow| &r.account);
        write_text(output, 3, &batch, |r: &NtfctnRow| &r.item_id);
        write_text(output, 5, &batch, |r: &NtfctnRow| &r.currency);
        write_text(output, 7, &batch, |r: &NtfctnRow| &r.debtor_name);
        write_text(output, 8, &batch, |r: &NtfctnRow| &r.purpose);
        write_text(output, 9, &batch, |r: &NtfctnRow| &r.source_file);
    }
}

// ── read_pain008 ─────────────────────────────────────────────────────────────

const DD_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("payment_info_id", Col::Text),
    ("payment_method", Col::Text),
    // FRST/RCUR/OOFF/FNAL — where this collection sits in the mandate's life.
    ("sequence_type", Col::Text),
    ("requested_collection_date", Col::Date),
    // The collector: pain.008 puts the CREDITOR on the payment group and one
    // debtor per transaction — pain.001 mirrored.
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("creditor_scheme_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("charge_bearer", Col::Text),
    // The debtor's signed authorisation — what makes the pull legal.
    ("mandate_id", Col::Text),
    ("mandate_signed_on", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain008, DdInit, DdStream<Source>, DdRow,
    name = "read_pain008",
    columns = DD_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 13, &batch, |r: &DdRow| r.amount);
        write_date(output, 5, &batch, |r: &DdRow| {
            r.requested_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 17, &batch, |r: &DdRow| {
            r.mandate_signed_on.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &DdRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &DdRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &DdRow| &r.payment_info_id);
        write_text(output, 3, &batch, |r: &DdRow| &r.payment_method);
        write_text(output, 4, &batch, |r: &DdRow| &r.sequence_type);
        write_text(output, 6, &batch, |r: &DdRow| &r.creditor_name);
        write_text(output, 7, &batch, |r: &DdRow| &r.creditor_account);
        write_text(output, 8, &batch, |r: &DdRow| &r.creditor_agent_bic);
        write_text(output, 9, &batch, |r: &DdRow| &r.creditor_scheme_id);
        write_text(output, 10, &batch, |r: &DdRow| &r.instr_id);
        write_text(output, 11, &batch, |r: &DdRow| &r.end_to_end_id);
        write_text(output, 12, &batch, |r: &DdRow| &r.uetr);
        write_text(output, 14, &batch, |r: &DdRow| &r.currency);
        write_text(output, 15, &batch, |r: &DdRow| &r.charge_bearer);
        write_text(output, 16, &batch, |r: &DdRow| &r.mandate_id);
        write_text(output, 18, &batch, |r: &DdRow| &r.debtor_name);
        write_text(output, 19, &batch, |r: &DdRow| &r.debtor_account);
        write_text(output, 20, &batch, |r: &DdRow| &r.debtor_agent_bic);
        write_text(output, 21, &batch, |r: &DdRow| &r.remittance_info);
        write_text(output, 22, &batch, |r: &DdRow| &r.source_file);
    }
}

// ── read_pain009 ─────────────────────────────────────────────────────────────

const MNDT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("created", Col::Stamp),
    ("initiating_party", Col::Text),
    ("mandate_id", Col::Text),
    // A mandate not yet registered has no id, only the id of the request.
    ("mandate_request_id", Col::Text),
    // FRST/RCUR/OOFF/FNAL and how often — what pain.008 later restates per
    // collection.
    ("sequence_type", Col::Text),
    ("frequency", Col::Text),
    ("first_collection_date", Col::Date),
    ("final_collection_date", Col::Date),
    // The fixed amount each collection may take, when the mandate caps it.
    ("collection_amount", Col::Money),
    ("currency", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("ultimate_debtor_name", Col::Text),
    ("referred_document_number", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain009, MndtInit, MndtStream<Source>, MndtRow,
    name = "read_pain009",
    columns = MNDT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 9, &batch, |r: &MndtRow| r.collection_amount);
        write_timestamp(output, 1, &batch, |r: &MndtRow| {
            r.created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 7, &batch, |r: &MndtRow| {
            r.first_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 8, &batch, |r: &MndtRow| {
            r.final_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &MndtRow| &r.msg_id);
        write_text(output, 2, &batch, |r: &MndtRow| &r.initiating_party);
        write_text(output, 3, &batch, |r: &MndtRow| &r.mandate_id);
        write_text(output, 4, &batch, |r: &MndtRow| &r.mandate_request_id);
        write_text(output, 5, &batch, |r: &MndtRow| &r.sequence_type);
        write_text(output, 6, &batch, |r: &MndtRow| &r.frequency);
        write_text(output, 10, &batch, |r: &MndtRow| &r.currency);
        write_text(output, 11, &batch, |r: &MndtRow| &r.creditor_name);
        write_text(output, 12, &batch, |r: &MndtRow| &r.creditor_account);
        write_text(output, 13, &batch, |r: &MndtRow| &r.creditor_agent_bic);
        write_text(output, 14, &batch, |r: &MndtRow| &r.debtor_name);
        write_text(output, 15, &batch, |r: &MndtRow| &r.debtor_account);
        write_text(output, 16, &batch, |r: &MndtRow| &r.debtor_agent_bic);
        write_text(output, 17, &batch, |r: &MndtRow| &r.ultimate_debtor_name);
        write_text(output, 18, &batch, |r: &MndtRow| &r.referred_document_number);
        write_text(output, 19, &batch, |r: &MndtRow| &r.source_file);
    }
}

// ── read_pain010 ─────────────────────────────────────────────────────────────

const AMDMNT_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("created", Col::Stamp),
    ("initiating_party", Col::Text),
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    ("amendment_reason", Col::Text),
    ("amendment_originator", Col::Text),
    // The mandate being changed; every column below is what it BECOMES.
    ("original_mandate_id", Col::Text),
    ("mandate_id", Col::Text),
    ("sequence_type", Col::Text),
    ("frequency", Col::Text),
    ("collection_amount", Col::Money),
    ("currency", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain010, AmdmntInit, AmdmntStream<Source>, AmdmntRow,
    name = "read_pain010",
    columns = AMDMNT_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 11, &batch, |r: &AmdmntRow| r.collection_amount);
        write_timestamp(output, 1, &batch, |r: &AmdmntRow| {
            r.created.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &AmdmntRow| &r.msg_id);
        write_text(output, 2, &batch, |r: &AmdmntRow| &r.initiating_party);
        write_text(output, 3, &batch, |r: &AmdmntRow| &r.instructing_agent_bic);
        write_text(output, 4, &batch, |r: &AmdmntRow| &r.instructed_agent_bic);
        write_text(output, 5, &batch, |r: &AmdmntRow| &r.amendment_reason);
        write_text(output, 6, &batch, |r: &AmdmntRow| &r.amendment_originator);
        write_text(output, 7, &batch, |r: &AmdmntRow| &r.original_mandate_id);
        write_text(output, 8, &batch, |r: &AmdmntRow| &r.mandate_id);
        write_text(output, 9, &batch, |r: &AmdmntRow| &r.sequence_type);
        write_text(output, 10, &batch, |r: &AmdmntRow| &r.frequency);
        write_text(output, 12, &batch, |r: &AmdmntRow| &r.currency);
        write_text(output, 13, &batch, |r: &AmdmntRow| &r.creditor_name);
        write_text(output, 14, &batch, |r: &AmdmntRow| &r.creditor_account);
        write_text(output, 15, &batch, |r: &AmdmntRow| &r.debtor_name);
        write_text(output, 16, &batch, |r: &AmdmntRow| &r.debtor_account);
        write_text(output, 17, &batch, |r: &AmdmntRow| &r.debtor_agent_bic);
        write_text(output, 18, &batch, |r: &AmdmntRow| &r.source_file);
    }
}

// ── read_pain011 ─────────────────────────────────────────────────────────────

const MNDT_CXL_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("created", Col::Stamp),
    ("initiating_party", Col::Text),
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    ("cancellation_reason", Col::Text),
    // NARR means "the reason is in the text", so the text is a column.
    ("cancellation_reason_info", Col::Text),
    ("original_mandate_id", Col::Text),
    // Populated only when the sender repeated the mandate being cancelled;
    // naming it by id alone is legal and complete.
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("ultimate_debtor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain011, MndtCxlInit, MndtCxlStream<Source>, MndtCxlRow,
    name = "read_pain011",
    columns = MNDT_CXL_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 1, &batch, |r: &MndtCxlRow| {
            r.created.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &MndtCxlRow| &r.msg_id);
        write_text(output, 2, &batch, |r: &MndtCxlRow| &r.initiating_party);
        write_text(output, 3, &batch, |r: &MndtCxlRow| &r.instructing_agent_bic);
        write_text(output, 4, &batch, |r: &MndtCxlRow| &r.instructed_agent_bic);
        write_text(output, 5, &batch, |r: &MndtCxlRow| &r.cancellation_reason);
        write_text(output, 6, &batch, |r: &MndtCxlRow| &r.cancellation_reason_info);
        write_text(output, 7, &batch, |r: &MndtCxlRow| &r.original_mandate_id);
        write_text(output, 8, &batch, |r: &MndtCxlRow| &r.creditor_name);
        write_text(output, 9, &batch, |r: &MndtCxlRow| &r.creditor_account);
        write_text(output, 10, &batch, |r: &MndtCxlRow| &r.debtor_name);
        write_text(output, 11, &batch, |r: &MndtCxlRow| &r.debtor_account);
        write_text(output, 12, &batch, |r: &MndtCxlRow| &r.debtor_agent_bic);
        write_text(output, 13, &batch, |r: &MndtCxlRow| &r.ultimate_debtor_name);
        write_text(output, 14, &batch, |r: &MndtCxlRow| &r.source_file);
    }
}

// ── read_pain012 ─────────────────────────────────────────────────────────────

const ACCPTNC_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("created", Col::Stamp),
    ("initiating_party", Col::Text),
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    ("original_msg_id", Col::Text),
    // Which mandate message is answered: pain.009, pain.010 or pain.011.
    ("original_msg_name_id", Col::Text),
    ("original_created", Col::Stamp),
    // As the wire spelled it, like group_cancellation in read_camt056.
    ("accepted", Col::Text),
    ("rejection_reason", Col::Text),
    ("original_mandate_id", Col::Text),
    // Populated only when the report repeated the mandate.
    ("sequence_type", Col::Text),
    ("frequency", Col::Text),
    ("first_collection_date", Col::Date),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("referred_document_number", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain012, AccptncInit, AccptncStream<Source>, AccptncRow,
    name = "read_pain012",
    columns = ACCPTNC_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 1, &batch, |r: &AccptncRow| {
            r.created.as_deref().and_then(temporal::ts_micros)
        });
        write_timestamp(output, 7, &batch, |r: &AccptncRow| {
            r.original_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 13, &batch, |r: &AccptncRow| {
            r.first_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &AccptncRow| &r.msg_id);
        write_text(output, 2, &batch, |r: &AccptncRow| &r.initiating_party);
        write_text(output, 3, &batch, |r: &AccptncRow| &r.instructing_agent_bic);
        write_text(output, 4, &batch, |r: &AccptncRow| &r.instructed_agent_bic);
        write_text(output, 5, &batch, |r: &AccptncRow| &r.original_msg_id);
        write_text(output, 6, &batch, |r: &AccptncRow| &r.original_msg_name_id);
        write_text(output, 8, &batch, |r: &AccptncRow| &r.accepted);
        write_text(output, 9, &batch, |r: &AccptncRow| &r.rejection_reason);
        write_text(output, 10, &batch, |r: &AccptncRow| &r.original_mandate_id);
        write_text(output, 11, &batch, |r: &AccptncRow| &r.sequence_type);
        write_text(output, 12, &batch, |r: &AccptncRow| &r.frequency);
        write_text(output, 14, &batch, |r: &AccptncRow| &r.creditor_name);
        write_text(output, 15, &batch, |r: &AccptncRow| &r.creditor_account);
        write_text(output, 16, &batch, |r: &AccptncRow| &r.creditor_agent_bic);
        write_text(output, 17, &batch, |r: &AccptncRow| &r.debtor_name);
        write_text(output, 18, &batch, |r: &AccptncRow| &r.debtor_account);
        write_text(output, 19, &batch, |r: &AccptncRow| &r.debtor_agent_bic);
        write_text(output, 20, &batch, |r: &AccptncRow| &r.referred_document_number);
        write_text(output, 21, &batch, |r: &AccptncRow| &r.source_file);
    }
}

// ── read_pain013 ─────────────────────────────────────────────────────────────

const ACTVTN_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("payment_info_id", Col::Text),
    ("payment_method", Col::Text),
    // The transaction may name its own date; then it wins over the group's.
    ("requested_execution_date", Col::Date),
    // A request to pay expires, and the corpus states the hour it does.
    ("expiry_date", Col::Stamp),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("charge_bearer", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain013, ActvtnInit, ActvtnStream<Source>, ActvtnRow,
    name = "read_pain013",
    columns = ACTVTN_COLUMNS,
    write = |output, batch| {
        write_date(output, 4, &batch, |r: &ActvtnRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_timestamp(output, 5, &batch, |r: &ActvtnRow| {
            r.expiry_date.as_deref().and_then(temporal::ts_micros)
        });
        write_decimal(output, 12, &batch, |r: &ActvtnRow| r.amount);
        write_text(output, 0, &batch, |r: &ActvtnRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &ActvtnRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &ActvtnRow| &r.payment_info_id);
        write_text(output, 3, &batch, |r: &ActvtnRow| &r.payment_method);
        write_text(output, 6, &batch, |r: &ActvtnRow| &r.debtor_name);
        write_text(output, 7, &batch, |r: &ActvtnRow| &r.debtor_account);
        write_text(output, 8, &batch, |r: &ActvtnRow| &r.debtor_agent_bic);
        write_text(output, 9, &batch, |r: &ActvtnRow| &r.instr_id);
        write_text(output, 10, &batch, |r: &ActvtnRow| &r.end_to_end_id);
        write_text(output, 11, &batch, |r: &ActvtnRow| &r.uetr);
        write_text(output, 13, &batch, |r: &ActvtnRow| &r.currency);
        write_text(output, 14, &batch, |r: &ActvtnRow| &r.charge_bearer);
        write_text(output, 15, &batch, |r: &ActvtnRow| &r.creditor_name);
        write_text(output, 16, &batch, |r: &ActvtnRow| &r.creditor_account);
        write_text(output, 17, &batch, |r: &ActvtnRow| &r.creditor_agent_bic);
        write_text(output, 18, &batch, |r: &ActvtnRow| &r.remittance_info);
        write_text(output, 19, &batch, |r: &ActvtnRow| &r.source_file);
    }
}

// ── read_pain014 ─────────────────────────────────────────────────────────────

const ACTVTN_STS_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("initiating_party", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    // GROUP, PAYMENT_INFO or TRANSACTION, as in read_pain002: only TRANSACTION
    // rows carry an amount.
    ("status_level", Col::Text),
    ("original_payment_info_id", Col::Text),
    ("status_id", Col::Text),
    ("status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    ("original_number_of_txs", Col::Text),
    ("original_control_sum", Col::Money),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("requested_execution_date", Col::Date),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("acceptance_date_time", Col::Stamp),
    ("source_file", Col::Text),
];

table_function! {
    ReadPain014, ActvtnStsInit, ActvtnStsStream<Source>, ActvtnStsRow,
    name = "read_pain014",
    columns = ACTVTN_STS_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 12, &batch, |r: &ActvtnStsRow| r.original_control_sum);
        write_decimal(output, 16, &batch, |r: &ActvtnStsRow| r.amount);
        write_date(output, 18, &batch, |r: &ActvtnStsRow| {
            r.requested_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_timestamp(output, 24, &batch, |r: &ActvtnStsRow| {
            r.acceptance_date_time
                .as_deref()
                .and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &ActvtnStsRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &ActvtnStsRow| &r.initiating_party);
        write_text(output, 2, &batch, |r: &ActvtnStsRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &ActvtnStsRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &ActvtnStsRow| &r.status_level);
        write_text(output, 5, &batch, |r: &ActvtnStsRow| {
            &r.original_payment_info_id
        });
        write_text(output, 6, &batch, |r: &ActvtnStsRow| &r.status_id);
        write_text(output, 7, &batch, |r: &ActvtnStsRow| &r.status);
        write_text(output, 8, &batch, |r: &ActvtnStsRow| &r.reason_code);
        write_text(output, 9, &batch, |r: &ActvtnStsRow| &r.reason_info);
        write_text(output, 10, &batch, |r: &ActvtnStsRow| &r.reason_originator);
        write_text(output, 11, &batch, |r: &ActvtnStsRow| {
            &r.original_number_of_txs
        });
        write_text(output, 13, &batch, |r: &ActvtnStsRow| &r.original_instr_id);
        write_text(output, 14, &batch, |r: &ActvtnStsRow| {
            &r.original_end_to_end_id
        });
        write_text(output, 15, &batch, |r: &ActvtnStsRow| &r.original_uetr);
        write_text(output, 17, &batch, |r: &ActvtnStsRow| &r.currency);
        write_text(output, 19, &batch, |r: &ActvtnStsRow| &r.debtor_name);
        write_text(output, 20, &batch, |r: &ActvtnStsRow| &r.debtor_account);
        write_text(output, 21, &batch, |r: &ActvtnStsRow| &r.creditor_name);
        write_text(output, 22, &batch, |r: &ActvtnStsRow| &r.creditor_account);
        write_text(output, 23, &batch, |r: &ActvtnStsRow| &r.remittance_info);
        write_text(output, 25, &batch, |r: &ActvtnStsRow| &r.source_file);
    }
}

// ── read_pacs007 ─────────────────────────────────────────────────────────────

const RVSL_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("reversal_id", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    // What went back, and what had settled: as in pacs.004, a reversal with
    // charges kept is amount < original_amount.
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("settlement_date", Col::Date),
    ("charge_bearer", Col::Text),
    ("reversal_reason_code", Col::Text),
    ("reversal_reason_info", Col::Text),
    ("reversal_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_debtor_account", Col::Text),
    ("original_debtor_agent_bic", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("original_creditor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs007, RvslInit, RvslStream<Source>, RvslRow,
    name = "read_pacs007",
    columns = RVSL_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 8, &batch, |r: &RvslRow| r.amount);
        write_decimal(output, 10, &batch, |r: &RvslRow| r.original_amount);
        write_date(output, 12, &batch, |r: &RvslRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RvslRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &RvslRow| &r.reversal_id);
        write_text(output, 2, &batch, |r: &RvslRow| &r.original_msg_id);
        write_text(output, 3, &batch, |r: &RvslRow| &r.original_msg_name_id);
        write_text(output, 4, &batch, |r: &RvslRow| &r.original_instr_id);
        write_text(output, 5, &batch, |r: &RvslRow| &r.original_end_to_end_id);
        write_text(output, 6, &batch, |r: &RvslRow| &r.original_tx_id);
        write_text(output, 7, &batch, |r: &RvslRow| &r.original_uetr);
        write_text(output, 9, &batch, |r: &RvslRow| &r.currency);
        write_text(output, 11, &batch, |r: &RvslRow| &r.original_currency);
        write_text(output, 13, &batch, |r: &RvslRow| &r.charge_bearer);
        write_text(output, 14, &batch, |r: &RvslRow| &r.reversal_reason_code);
        write_text(output, 15, &batch, |r: &RvslRow| &r.reversal_reason_info);
        write_text(output, 16, &batch, |r: &RvslRow| &r.reversal_originator);
        write_text(output, 17, &batch, |r: &RvslRow| &r.original_debtor_name);
        write_text(output, 18, &batch, |r: &RvslRow| &r.original_debtor_account);
        write_text(output, 19, &batch, |r: &RvslRow| &r.original_debtor_agent_bic);
        write_text(output, 20, &batch, |r: &RvslRow| &r.original_creditor_name);
        write_text(output, 21, &batch, |r: &RvslRow| &r.original_creditor_account);
        write_text(output, 22, &batch, |r: &RvslRow| &r.original_creditor_agent_bic);
        write_text(output, 23, &batch, |r: &RvslRow| &r.remittance_info);
        write_text(output, 24, &batch, |r: &RvslRow| &r.source_file);
    }
}

// ── read_camt055 ─────────────────────────────────────────────────────────────

const CCL_COLUMNS: &[(&str, Col)] = &[
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    // Usually a customer party, not a bank: this is the customer-side request.
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    // GROUP, PAYMENT_INFO or TRANSACTION — the pain-side has all three levels.
    ("scope", Col::Text),
    ("cancellation_id", Col::Text),
    ("group_cancellation", Col::Text),
    ("original_number_of_txs", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_payment_info_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_uetr", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    // Execution date on the pain.001 side, collection date on the pain.008 side.
    ("original_execution_date", Col::Date),
    ("cancellation_reason_code", Col::Text),
    ("cancellation_reason_info", Col::Text),
    ("cancellation_originator", Col::Text),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("original_creditor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt055, CclInit, CclStream<Source>, CclRow,
    name = "read_camt055",
    columns = CCL_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 14, &batch, |r: &CclRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &CclRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 16, &batch, |r: &CclRow| {
            r.original_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &CclRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &CclRow| &r.assigner);
        write_text(output, 3, &batch, |r: &CclRow| &r.assignee);
        write_text(output, 4, &batch, |r: &CclRow| &r.scope);
        write_text(output, 5, &batch, |r: &CclRow| &r.cancellation_id);
        write_text(output, 6, &batch, |r: &CclRow| &r.group_cancellation);
        write_text(output, 7, &batch, |r: &CclRow| &r.original_number_of_txs);
        write_text(output, 8, &batch, |r: &CclRow| &r.original_msg_id);
        write_text(output, 9, &batch, |r: &CclRow| &r.original_msg_name_id);
        write_text(output, 10, &batch, |r: &CclRow| &r.original_payment_info_id);
        write_text(output, 11, &batch, |r: &CclRow| &r.original_instr_id);
        write_text(output, 12, &batch, |r: &CclRow| &r.original_end_to_end_id);
        write_text(output, 13, &batch, |r: &CclRow| &r.original_uetr);
        write_text(output, 15, &batch, |r: &CclRow| &r.original_currency);
        write_text(output, 17, &batch, |r: &CclRow| &r.cancellation_reason_code);
        write_text(output, 18, &batch, |r: &CclRow| &r.cancellation_reason_info);
        write_text(output, 19, &batch, |r: &CclRow| &r.cancellation_originator);
        write_text(output, 20, &batch, |r: &CclRow| &r.original_debtor_name);
        write_text(output, 21, &batch, |r: &CclRow| &r.original_creditor_name);
        write_text(output, 22, &batch, |r: &CclRow| &r.original_creditor_account);
        write_text(output, 23, &batch, |r: &CclRow| &r.remittance_info);
        write_text(output, 24, &batch, |r: &CclRow| &r.source_file);
    }
}

// ── read_pacs003 ─────────────────────────────────────────────────────────────

const DDI_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("tx_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("settlement_date", Col::Date),
    ("requested_collection_date", Col::Date),
    // A batch is typically all-FRST or all-RCUR, so the wire states it once on
    // the group header; a transaction may restate it.
    ("sequence_type", Col::Text),
    ("charge_bearer", Col::Text),
    // The mandate travels with the collection: the debtor's bank may check it
    // before letting money leave the account.
    ("mandate_id", Col::Text),
    ("mandate_signed_on", Col::Date),
    ("creditor_name", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("debtor_name", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs003, DdiInit, DdiStream<Source>, DdiRow,
    name = "read_pacs003",
    columns = DDI_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 5, &batch, |r: &DdiRow| r.amount);
        write_date(output, 7, &batch, |r: &DdiRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_date(output, 8, &batch, |r: &DdiRow| {
            r.requested_collection_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 12, &batch, |r: &DdiRow| {
            r.mandate_signed_on.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &DdiRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &DdiRow| &r.instr_id);
        write_text(output, 2, &batch, |r: &DdiRow| &r.end_to_end_id);
        write_text(output, 3, &batch, |r: &DdiRow| &r.tx_id);
        write_text(output, 4, &batch, |r: &DdiRow| &r.uetr);
        write_text(output, 6, &batch, |r: &DdiRow| &r.currency);
        write_text(output, 9, &batch, |r: &DdiRow| &r.sequence_type);
        write_text(output, 10, &batch, |r: &DdiRow| &r.charge_bearer);
        write_text(output, 11, &batch, |r: &DdiRow| &r.mandate_id);
        write_text(output, 13, &batch, |r: &DdiRow| &r.creditor_name);
        write_text(output, 14, &batch, |r: &DdiRow| &r.creditor_account);
        write_text(output, 15, &batch, |r: &DdiRow| &r.creditor_agent_bic);
        write_text(output, 16, &batch, |r: &DdiRow| &r.debtor_name);
        write_text(output, 17, &batch, |r: &DdiRow| &r.debtor_account);
        write_text(output, 18, &batch, |r: &DdiRow| &r.debtor_agent_bic);
        write_text(output, 19, &batch, |r: &DdiRow| &r.remittance_info);
        write_text(output, 20, &batch, |r: &DdiRow| &r.source_file);
    }
}

// ── read_pacs009 ─────────────────────────────────────────────────────────────

const FI_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("tx_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("settlement_date", Col::Date),
    // The parties of a pacs.009 are banks, not customers.
    ("debtor_fi", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("creditor_fi", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    // COV: the customer transfer this cover payment settles — who the money is
    // really for. Hiding these is what MT202COV was invented to stop.
    ("underlying_debtor_name", Col::Text),
    ("underlying_debtor_account", Col::Text),
    ("underlying_creditor_name", Col::Text),
    ("underlying_creditor_account", Col::Text),
    ("underlying_remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs009, FiInit, FiStream<Source>, FiRow,
    name = "read_pacs009",
    columns = FI_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 5, &batch, |r: &FiRow| r.amount);
        write_date(output, 7, &batch, |r: &FiRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &FiRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &FiRow| &r.instr_id);
        write_text(output, 2, &batch, |r: &FiRow| &r.end_to_end_id);
        write_text(output, 3, &batch, |r: &FiRow| &r.tx_id);
        write_text(output, 4, &batch, |r: &FiRow| &r.uetr);
        write_text(output, 6, &batch, |r: &FiRow| &r.currency);
        write_text(output, 8, &batch, |r: &FiRow| &r.debtor_fi);
        write_text(output, 9, &batch, |r: &FiRow| &r.debtor_account);
        write_text(output, 10, &batch, |r: &FiRow| &r.debtor_agent_bic);
        write_text(output, 11, &batch, |r: &FiRow| &r.creditor_fi);
        write_text(output, 12, &batch, |r: &FiRow| &r.creditor_account);
        write_text(output, 13, &batch, |r: &FiRow| &r.creditor_agent_bic);
        write_text(output, 14, &batch, |r: &FiRow| &r.underlying_debtor_name);
        write_text(output, 15, &batch, |r: &FiRow| &r.underlying_debtor_account);
        write_text(output, 16, &batch, |r: &FiRow| &r.underlying_creditor_name);
        write_text(output, 17, &batch, |r: &FiRow| &r.underlying_creditor_account);
        write_text(output, 18, &batch, |r: &FiRow| &r.underlying_remittance_info);
        write_text(output, 19, &batch, |r: &FiRow| &r.source_file);
    }
}

// ── read_pacs010 ─────────────────────────────────────────────────────────────

const FI_DD_COLUMNS: &[(&str, Col)] = &[
    ("msg_id", Col::Text),
    // The credit instruction is the mid level: one creditor collecting, many
    // debtors. Its context is carried into every transaction beneath it.
    ("credit_instruction_id", Col::Text),
    ("instructing_agent_bic", Col::Text),
    ("instructed_agent_bic", Col::Text),
    // The parties of a pacs.010 are banks, not customers.
    ("creditor_fi", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_agent_bic", Col::Text),
    ("instr_id", Col::Text),
    ("end_to_end_id", Col::Text),
    ("tx_id", Col::Text),
    ("uetr", Col::Text),
    ("amount", Col::Money),
    ("currency", Col::Text),
    ("settlement_date", Col::Date),
    ("debtor_fi", Col::Text),
    ("debtor_account", Col::Text),
    ("debtor_agent_bic", Col::Text),
    ("purpose", Col::Text),
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadPacs010, FiDdInit, FiDdStream<Source>, FiDdRow,
    name = "read_pacs010",
    columns = FI_DD_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 11, &batch, |r: &FiDdRow| r.amount);
        write_date(output, 13, &batch, |r: &FiDdRow| {
            r.settlement_date.as_deref().and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &FiDdRow| &r.msg_id);
        write_text(output, 1, &batch, |r: &FiDdRow| &r.credit_instruction_id);
        write_text(output, 2, &batch, |r: &FiDdRow| &r.instructing_agent_bic);
        write_text(output, 3, &batch, |r: &FiDdRow| &r.instructed_agent_bic);
        write_text(output, 4, &batch, |r: &FiDdRow| &r.creditor_fi);
        write_text(output, 5, &batch, |r: &FiDdRow| &r.creditor_account);
        write_text(output, 6, &batch, |r: &FiDdRow| &r.creditor_agent_bic);
        write_text(output, 7, &batch, |r: &FiDdRow| &r.instr_id);
        write_text(output, 8, &batch, |r: &FiDdRow| &r.end_to_end_id);
        write_text(output, 9, &batch, |r: &FiDdRow| &r.tx_id);
        write_text(output, 10, &batch, |r: &FiDdRow| &r.uetr);
        write_text(output, 12, &batch, |r: &FiDdRow| &r.currency);
        write_text(output, 14, &batch, |r: &FiDdRow| &r.debtor_fi);
        write_text(output, 15, &batch, |r: &FiDdRow| &r.debtor_account);
        write_text(output, 16, &batch, |r: &FiDdRow| &r.debtor_agent_bic);
        write_text(output, 17, &batch, |r: &FiDdRow| &r.purpose);
        write_text(output, 18, &batch, |r: &FiDdRow| &r.remittance_info);
        write_text(output, 19, &batch, |r: &FiDdRow| &r.source_file);
    }
}

// ── read_camt029 ─────────────────────────────────────────────────────────────

const ROI_COLUMNS: &[(&str, Col)] = &[
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    // RESOLUTION (the message-level answer), GROUP, or TRANSACTION. Most real
    // camt.029 files answer at message level only.
    ("scope", Col::Text),
    // CNCL cancelled, RJCR cancellation rejected, … — on the RESOLUTION row.
    ("resolution_status", Col::Text),
    ("case_id", Col::Text),
    ("cancellation_status_id", Col::Text),
    ("cancellation_status", Col::Text),
    ("reason_code", Col::Text),
    ("reason_info", Col::Text),
    ("reason_originator", Col::Text),
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_tx_id", Col::Text),
    ("original_uetr", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_settlement_date", Col::Date),
    ("original_debtor_name", Col::Text),
    ("original_creditor_name", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt029, RoiInit, RoiStream<Source>, RoiRow,
    name = "read_camt029",
    columns = ROI_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 18, &batch, |r: &RoiRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &RoiRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 20, &batch, |r: &RoiRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &RoiRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &RoiRow| &r.assigner);
        write_text(output, 3, &batch, |r: &RoiRow| &r.assignee);
        write_text(output, 4, &batch, |r: &RoiRow| &r.scope);
        write_text(output, 5, &batch, |r: &RoiRow| &r.resolution_status);
        write_text(output, 6, &batch, |r: &RoiRow| &r.case_id);
        write_text(output, 7, &batch, |r: &RoiRow| &r.cancellation_status_id);
        write_text(output, 8, &batch, |r: &RoiRow| &r.cancellation_status);
        write_text(output, 9, &batch, |r: &RoiRow| &r.reason_code);
        write_text(output, 10, &batch, |r: &RoiRow| &r.reason_info);
        write_text(output, 11, &batch, |r: &RoiRow| &r.reason_originator);
        write_text(output, 12, &batch, |r: &RoiRow| &r.original_msg_id);
        write_text(output, 13, &batch, |r: &RoiRow| &r.original_msg_name_id);
        write_text(output, 14, &batch, |r: &RoiRow| &r.original_instr_id);
        write_text(output, 15, &batch, |r: &RoiRow| &r.original_end_to_end_id);
        write_text(output, 16, &batch, |r: &RoiRow| &r.original_tx_id);
        write_text(output, 17, &batch, |r: &RoiRow| &r.original_uetr);
        write_text(output, 19, &batch, |r: &RoiRow| &r.original_currency);
        write_text(output, 21, &batch, |r: &RoiRow| &r.original_debtor_name);
        write_text(output, 22, &batch, |r: &RoiRow| &r.original_creditor_name);
        write_text(output, 23, &batch, |r: &RoiRow| &r.source_file);
    }
}

// ── read_camt027 ─────────────────────────────────────────────────────────────

/// The six columns every investigation reader starts with: who is asking whom,
/// and which case it belongs to.
const CASE_COLUMNS: [(&str, Col); 6] = [
    ("assignment_id", Col::Text),
    ("assignment_created", Col::Stamp),
    ("assigner", Col::Text),
    ("assignee", Col::Text),
    ("case_id", Col::Text),
    ("case_creator", Col::Text),
];

const CLAIM_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    // A claim moves no money: every monetary column is the missing payment's.
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    // Whichever side the sender stated: the initiation's date, or the
    // interbank settlement's.
    ("original_execution_date", Col::Date),
    ("original_settlement_date", Col::Date),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt027, ClaimInit, ClaimStream<Source>, ClaimRow,
    name = "read_camt027",
    columns = CLAIM_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 9, &batch, |r: &ClaimRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &ClaimRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 11, &batch, |r: &ClaimRow| {
            r.original_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 12, &batch, |r: &ClaimRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &ClaimRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &ClaimRow| &r.assigner);
        write_text(output, 3, &batch, |r: &ClaimRow| &r.assignee);
        write_text(output, 4, &batch, |r: &ClaimRow| &r.case_id);
        write_text(output, 5, &batch, |r: &ClaimRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &ClaimRow| &r.original_msg_id);
        write_text(output, 7, &batch, |r: &ClaimRow| &r.original_msg_name_id);
        write_text(output, 8, &batch, |r: &ClaimRow| &r.original_instr_id);
        write_text(output, 10, &batch, |r: &ClaimRow| &r.original_currency);
        write_text(output, 13, &batch, |r: &ClaimRow| &r.source_file);
    }
}

// ── read_camt028 ─────────────────────────────────────────────────────────────

const ADDTL_INF_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    // The published samples name the payment by instruction id only, never by
    // the original message id.
    ("original_instr_id", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_execution_date", Col::Date),
    ("original_settlement_date", Col::Date),
    // What the investigation was missing, which is why this message exists.
    ("remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt028, AddtlInfInit, AddtlInfStream<Source>, AddtlInfRow,
    name = "read_camt028",
    columns = ADDTL_INF_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 7, &batch, |r: &AddtlInfRow| r.original_amount);
        write_timestamp(output, 1, &batch, |r: &AddtlInfRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 9, &batch, |r: &AddtlInfRow| {
            r.original_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 10, &batch, |r: &AddtlInfRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &AddtlInfRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &AddtlInfRow| &r.assigner);
        write_text(output, 3, &batch, |r: &AddtlInfRow| &r.assignee);
        write_text(output, 4, &batch, |r: &AddtlInfRow| &r.case_id);
        write_text(output, 5, &batch, |r: &AddtlInfRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &AddtlInfRow| &r.original_instr_id);
        write_text(output, 8, &batch, |r: &AddtlInfRow| &r.original_currency);
        write_text(output, 11, &batch, |r: &AddtlInfRow| &r.remittance_info);
        write_text(output, 12, &batch, |r: &AddtlInfRow| &r.source_file);
    }
}

// ── read_camt030 ─────────────────────────────────────────────────────────────

const CASE_NTFCTN_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    // The SECOND party pair: who is being told, by whom. It need not be the
    // assignment's pair, and in the real sample it is not.
    ("notification_id", Col::Text),
    ("notification_from", Col::Text),
    ("notification_to", Col::Text),
    ("notification_created", Col::Stamp),
    // Why the case moved: a bare code (CANC, FTHI, MINE).
    ("justification", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt030, CaseNtfctnInit, CaseNtfctnStream<Source>, CaseNtfctnRow,
    name = "read_camt030",
    columns = CASE_NTFCTN_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 1, &batch, |r: &CaseNtfctnRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_timestamp(output, 9, &batch, |r: &CaseNtfctnRow| {
            r.notification_created
                .as_deref()
                .and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &CaseNtfctnRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &CaseNtfctnRow| &r.assigner);
        write_text(output, 3, &batch, |r: &CaseNtfctnRow| &r.assignee);
        write_text(output, 4, &batch, |r: &CaseNtfctnRow| &r.case_id);
        write_text(output, 5, &batch, |r: &CaseNtfctnRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &CaseNtfctnRow| &r.notification_id);
        write_text(output, 7, &batch, |r: &CaseNtfctnRow| &r.notification_from);
        write_text(output, 8, &batch, |r: &CaseNtfctnRow| &r.notification_to);
        write_text(output, 10, &batch, |r: &CaseNtfctnRow| &r.justification);
        write_text(output, 11, &batch, |r: &CaseNtfctnRow| &r.source_file);
    }
}

// ── read_camt031 ─────────────────────────────────────────────────────────────

const RJCT_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    // NFND: the payment named in the case was not found.
    ("rejection_reason", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt031, RjctInit, RjctStream<Source>, RjctRow,
    name = "read_camt031",
    columns = RJCT_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 1, &batch, |r: &RjctRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &RjctRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &RjctRow| &r.assigner);
        write_text(output, 3, &batch, |r: &RjctRow| &r.assignee);
        write_text(output, 4, &batch, |r: &RjctRow| &r.case_id);
        write_text(output, 5, &batch, |r: &RjctRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &RjctRow| &r.rejection_reason);
        write_text(output, 7, &batch, |r: &RjctRow| &r.source_file);
    }
}

// ── read_camt036 ─────────────────────────────────────────────────────────────

const DBT_RSPN_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    // As the wire spelled it; "true" means the debit may go ahead.
    ("debit_authorised", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt036, DbtRspnInit, DbtRspnStream<Source>, DbtRspnRow,
    name = "read_camt036",
    columns = DBT_RSPN_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 1, &batch, |r: &DbtRspnRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_text(output, 0, &batch, |r: &DbtRspnRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &DbtRspnRow| &r.assigner);
        write_text(output, 3, &batch, |r: &DbtRspnRow| &r.assignee);
        write_text(output, 4, &batch, |r: &DbtRspnRow| &r.case_id);
        write_text(output, 5, &batch, |r: &DbtRspnRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &DbtRspnRow| &r.debit_authorised);
        write_text(output, 7, &batch, |r: &DbtRspnRow| &r.source_file);
    }
}

// ── read_camt037 ─────────────────────────────────────────────────────────────

const DBT_REQ_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    ("original_instr_id", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_execution_date", Col::Date),
    ("original_settlement_date", Col::Date),
    ("cancellation_reason", Col::Text),
    // What is being asked for, which is at most the original: a bank that kept
    // its charges asks for less than it paid out.
    ("amount_to_debit", Col::Money),
    ("debit_currency", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt037, DbtReqInit, DbtReqStream<Source>, DbtReqRow,
    name = "read_camt037",
    columns = DBT_REQ_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 7, &batch, |r: &DbtReqRow| r.original_amount);
        write_decimal(output, 12, &batch, |r: &DbtReqRow| r.amount_to_debit);
        write_timestamp(output, 1, &batch, |r: &DbtReqRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 9, &batch, |r: &DbtReqRow| {
            r.original_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 10, &batch, |r: &DbtReqRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &DbtReqRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &DbtReqRow| &r.assigner);
        write_text(output, 3, &batch, |r: &DbtReqRow| &r.assignee);
        write_text(output, 4, &batch, |r: &DbtReqRow| &r.case_id);
        write_text(output, 5, &batch, |r: &DbtReqRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &DbtReqRow| &r.original_instr_id);
        write_text(output, 8, &batch, |r: &DbtReqRow| &r.original_currency);
        write_text(output, 11, &batch, |r: &DbtReqRow| &r.cancellation_reason);
        write_text(output, 13, &batch, |r: &DbtReqRow| &r.debit_currency);
        write_text(output, 14, &batch, |r: &DbtReqRow| &r.source_file);
    }
}

// ── read_camt087 ─────────────────────────────────────────────────────────────

const MODFY_COLUMNS: &[(&str, Col)] = &[
    CASE_COLUMNS[0],
    CASE_COLUMNS[1],
    CASE_COLUMNS[2],
    CASE_COLUMNS[3],
    CASE_COLUMNS[4],
    CASE_COLUMNS[5],
    ("original_msg_id", Col::Text),
    ("original_msg_name_id", Col::Text),
    ("original_instr_id", Col::Text),
    ("original_end_to_end_id", Col::Text),
    ("original_amount", Col::Money),
    ("original_currency", Col::Text),
    ("original_execution_date", Col::Date),
    ("original_settlement_date", Col::Date),
    // What the payment should become; the difference from the original is a
    // subtraction rather than a second query.
    ("modified_amount", Col::Money),
    ("modified_currency", Col::Text),
    ("modified_remittance_info", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadCamt087, ModfyInit, ModfyStream<Source>, ModfyRow,
    name = "read_camt087",
    columns = MODFY_COLUMNS,
    write = |output, batch| {
        write_decimal(output, 10, &batch, |r: &ModfyRow| r.original_amount);
        write_decimal(output, 14, &batch, |r: &ModfyRow| r.modified_amount);
        write_timestamp(output, 1, &batch, |r: &ModfyRow| {
            r.assignment_created.as_deref().and_then(temporal::ts_micros)
        });
        write_date(output, 12, &batch, |r: &ModfyRow| {
            r.original_execution_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_date(output, 13, &batch, |r: &ModfyRow| {
            r.original_settlement_date
                .as_deref()
                .and_then(temporal::date_days)
        });
        write_text(output, 0, &batch, |r: &ModfyRow| &r.assignment_id);
        write_text(output, 2, &batch, |r: &ModfyRow| &r.assigner);
        write_text(output, 3, &batch, |r: &ModfyRow| &r.assignee);
        write_text(output, 4, &batch, |r: &ModfyRow| &r.case_id);
        write_text(output, 5, &batch, |r: &ModfyRow| &r.case_creator);
        write_text(output, 6, &batch, |r: &ModfyRow| &r.original_msg_id);
        write_text(output, 7, &batch, |r: &ModfyRow| &r.original_msg_name_id);
        write_text(output, 8, &batch, |r: &ModfyRow| &r.original_instr_id);
        write_text(output, 9, &batch, |r: &ModfyRow| &r.original_end_to_end_id);
        write_text(output, 11, &batch, |r: &ModfyRow| &r.original_currency);
        write_text(output, 15, &batch, |r: &ModfyRow| &r.modified_currency);
        write_text(output, 16, &batch, |r: &ModfyRow| &r.modified_remittance_info);
        write_text(output, 17, &batch, |r: &ModfyRow| &r.source_file);
    }
}

// ── read_mt101 ──────────────────────────────────────────────────────────────

const MT101_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    ("sender_reference", Col::Text),
    ("customer_reference", Col::Text),
    // :28D: is this message's place in a series a bank split a batch across.
    ("message_index", Col::Int),
    ("message_total", Col::Int),
    ("requested_execution_date", Col::Date),
    ("authorisation", Col::Text),
    ("sending_institution", Col::Text),
    // Options C and L of field 50a name whoever instructed the bank; F, G and H
    // name the customer whose account pays. Both may be stated, and `50a` may sit
    // in the header or in each transaction, so these are the effective values.
    ("instructing_party", Col::Text),
    ("party_option_50", Col::Text),
    ("ordering_customer", Col::Text),
    ("ordering_customer_account", Col::Text),
    ("account_servicing_institution", Col::Text),
    ("account_servicing_institution_account", Col::Text),
    ("tx_ref", Col::Text),
    ("fx_deal_ref", Col::Text),
    ("instruction_codes", Col::Text),
    ("currency", Col::Text),
    ("amount", Col::Money),
    ("instructed_currency", Col::Text),
    ("instructed_amount", Col::Money),
    ("exchange_rate", Col::Text),
    ("intermediary_institution", Col::Text),
    ("account_with_institution", Col::Text),
    ("account_with_institution_account", Col::Text),
    ("party_option_59", Col::Text),
    ("beneficiary", Col::Text),
    ("beneficiary_account", Col::Text),
    ("remittance_info", Col::Text),
    ("details_of_charges", Col::Text),
    ("charges_account", Col::Text),
    ("regulatory_reporting", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt101, Mt101Init, Mt101Stream<Source>, Mt101Row,
    name = "read_mt101",
    columns = MT101_COLUMNS,
    write = |output, batch| {
        write_bigint(output, 9, &batch, |r: &Mt101Row| r.message_index);
        write_bigint(output, 10, &batch, |r: &Mt101Row| r.message_total);
        write_date(output, 11, &batch, |r: &Mt101Row| r.requested_execution_date);
        write_decimal(output, 24, &batch, |r: &Mt101Row| r.amount);
        write_decimal(output, 26, &batch, |r: &Mt101Row| r.instructed_amount);
        write_text(output, 0, &batch, |r: &Mt101Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt101Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt101Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt101Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt101Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt101Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt101Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt101Row| &r.sender_reference);
        write_text(output, 8, &batch, |r: &Mt101Row| &r.customer_reference);
        write_text(output, 12, &batch, |r: &Mt101Row| &r.authorisation);
        write_text(output, 13, &batch, |r: &Mt101Row| &r.sending_institution);
        write_text(output, 14, &batch, |r: &Mt101Row| &r.instructing_party);
        write_text(output, 15, &batch, |r: &Mt101Row| &r.party_option_50);
        write_text(output, 16, &batch, |r: &Mt101Row| &r.ordering_customer);
        write_text(output, 17, &batch, |r: &Mt101Row| &r.ordering_customer_account);
        write_text(output, 18, &batch, |r: &Mt101Row| &r.account_servicing_institution);
        write_text(output, 19, &batch, |r: &Mt101Row| {
            &r.account_servicing_institution_account
        });
        write_text(output, 20, &batch, |r: &Mt101Row| &r.tx_ref);
        write_text(output, 21, &batch, |r: &Mt101Row| &r.fx_deal_ref);
        write_text(output, 22, &batch, |r: &Mt101Row| &r.instruction_codes);
        write_text(output, 23, &batch, |r: &Mt101Row| &r.currency);
        write_text(output, 25, &batch, |r: &Mt101Row| &r.instructed_currency);
        write_text(output, 27, &batch, |r: &Mt101Row| &r.exchange_rate);
        write_text(output, 28, &batch, |r: &Mt101Row| &r.intermediary_institution);
        write_text(output, 29, &batch, |r: &Mt101Row| &r.account_with_institution);
        write_text(output, 30, &batch, |r: &Mt101Row| {
            &r.account_with_institution_account
        });
        write_text(output, 31, &batch, |r: &Mt101Row| &r.party_option_59);
        write_text(output, 32, &batch, |r: &Mt101Row| &r.beneficiary);
        write_text(output, 33, &batch, |r: &Mt101Row| &r.beneficiary_account);
        write_text(output, 34, &batch, |r: &Mt101Row| &r.remittance_info);
        write_text(output, 35, &batch, |r: &Mt101Row| &r.details_of_charges);
        write_text(output, 36, &batch, |r: &Mt101Row| &r.charges_account);
        write_text(output, 37, &batch, |r: &Mt101Row| &r.regulatory_reporting);
        write_text(output, 38, &batch, |r: &Mt101Row| &r.source_file);
    }
}

// -- read_mt104 ---------------------------------------------------------------

const MT104_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    ("sender_reference", Col::Text),
    ("customer_reference", Col::Text),
    ("registration_reference", Col::Text),
    ("requested_execution_date", Col::Date),
    ("sending_institution", Col::Text),
    ("instructing_party", Col::Text),
    ("party_option_50", Col::Text),
    ("creditor", Col::Text),
    ("creditor_account", Col::Text),
    ("creditor_bank", Col::Text),
    ("creditor_bank_account", Col::Text),
    ("transaction_type_code", Col::Text),
    ("details_of_charges", Col::Text),
    ("regulatory_reporting", Col::Text),
    ("sender_to_receiver", Col::Text),
    ("tx_ref", Col::Text),
    ("instruction_codes", Col::Text),
    ("mandate_reference", Col::Text),
    ("direct_debit_reference", Col::Text),
    ("currency", Col::Text),
    ("amount", Col::Money),
    ("instructed_currency", Col::Text),
    ("instructed_amount", Col::Money),
    ("exchange_rate", Col::Text),
    ("debtor_bank", Col::Text),
    ("debtor_bank_account", Col::Text),
    ("party_option_59", Col::Text),
    ("debtor", Col::Text),
    ("debtor_account", Col::Text),
    ("remittance_info", Col::Text),
    ("senders_charges", Col::Text),
    ("receivers_charges", Col::Text),
    ("settlement_currency", Col::Text),
    ("settlement_amount", Col::Money),
    ("sum_of_amounts", Col::Money),
    ("sum_senders_charges", Col::Text),
    ("sum_receivers_charges", Col::Text),
    ("senders_correspondent", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt104, Mt104Init, Mt104Stream<Source>, Mt104Row,
    name = "read_mt104",
    columns = MT104_COLUMNS,
    write = |output, batch| {
        write_date(output, 10, &batch, |r: &Mt104Row| r.requested_execution_date);
        write_decimal(output, 27, &batch, |r: &Mt104Row| r.amount);
        write_decimal(output, 29, &batch, |r: &Mt104Row| r.instructed_amount);
        write_decimal(output, 40, &batch, |r: &Mt104Row| r.settlement_amount);
        write_decimal(output, 41, &batch, |r: &Mt104Row| r.sum_of_amounts);
        write_text(output, 0, &batch, |r: &Mt104Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt104Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt104Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt104Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt104Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt104Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt104Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt104Row| &r.sender_reference);
        write_text(output, 8, &batch, |r: &Mt104Row| &r.customer_reference);
        write_text(output, 9, &batch, |r: &Mt104Row| &r.registration_reference);
        write_text(output, 11, &batch, |r: &Mt104Row| &r.sending_institution);
        write_text(output, 12, &batch, |r: &Mt104Row| &r.instructing_party);
        write_text(output, 13, &batch, |r: &Mt104Row| &r.party_option_50);
        write_text(output, 14, &batch, |r: &Mt104Row| &r.creditor);
        write_text(output, 15, &batch, |r: &Mt104Row| &r.creditor_account);
        write_text(output, 16, &batch, |r: &Mt104Row| &r.creditor_bank);
        write_text(output, 17, &batch, |r: &Mt104Row| &r.creditor_bank_account);
        write_text(output, 18, &batch, |r: &Mt104Row| &r.transaction_type_code);
        write_text(output, 19, &batch, |r: &Mt104Row| &r.details_of_charges);
        write_text(output, 20, &batch, |r: &Mt104Row| &r.regulatory_reporting);
        write_text(output, 21, &batch, |r: &Mt104Row| &r.sender_to_receiver);
        write_text(output, 22, &batch, |r: &Mt104Row| &r.tx_ref);
        write_text(output, 23, &batch, |r: &Mt104Row| &r.instruction_codes);
        write_text(output, 24, &batch, |r: &Mt104Row| &r.mandate_reference);
        write_text(output, 25, &batch, |r: &Mt104Row| &r.direct_debit_reference);
        write_text(output, 26, &batch, |r: &Mt104Row| &r.currency);
        write_text(output, 28, &batch, |r: &Mt104Row| &r.instructed_currency);
        write_text(output, 30, &batch, |r: &Mt104Row| &r.exchange_rate);
        write_text(output, 31, &batch, |r: &Mt104Row| &r.debtor_bank);
        write_text(output, 32, &batch, |r: &Mt104Row| &r.debtor_bank_account);
        write_text(output, 33, &batch, |r: &Mt104Row| &r.party_option_59);
        write_text(output, 34, &batch, |r: &Mt104Row| &r.debtor);
        write_text(output, 35, &batch, |r: &Mt104Row| &r.debtor_account);
        write_text(output, 36, &batch, |r: &Mt104Row| &r.remittance_info);
        write_text(output, 37, &batch, |r: &Mt104Row| &r.senders_charges);
        write_text(output, 38, &batch, |r: &Mt104Row| &r.receivers_charges);
        write_text(output, 39, &batch, |r: &Mt104Row| &r.settlement_currency);
        write_text(output, 42, &batch, |r: &Mt104Row| &r.sum_senders_charges);
        write_text(output, 43, &batch, |r: &Mt104Row| &r.sum_receivers_charges);
        write_text(output, 44, &batch, |r: &Mt104Row| &r.senders_correspondent);
        write_text(output, 45, &batch, |r: &Mt104Row| &r.source_file);
    }
}

// ── read_mt103 ──────────────────────────────────────────────────────────────

const MT103_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    ("tx_ref", Col::Text),
    ("time_indications", Col::Text),
    ("bank_operation_code", Col::Text),
    ("instruction_codes", Col::Text),
    ("transaction_type_code", Col::Text),
    // :32A: carries the value date, the currency and the amount together.
    ("value_date", Col::Date),
    ("currency", Col::Text),
    ("amount", Col::Money),
    ("instructed_currency", Col::Text),
    ("instructed_amount", Col::Money),
    ("exchange_rate", Col::Text),
    // Which option letter the message chose for the ordering customer: A is a
    // BIC, F numbered name-and-address lines, K free-text name and address.
    ("party_option_50", Col::Text),
    ("ordering_customer", Col::Text),
    ("ordering_customer_account", Col::Text),
    ("sending_institution", Col::Text),
    ("ordering_institution", Col::Text),
    ("ordering_institution_account", Col::Text),
    ("senders_correspondent", Col::Text),
    ("senders_correspondent_account", Col::Text),
    ("receivers_correspondent", Col::Text),
    ("third_reimbursement_institution", Col::Text),
    ("intermediary_institution", Col::Text),
    ("account_with_institution", Col::Text),
    ("account_with_institution_account", Col::Text),
    ("party_option_59", Col::Text),
    ("beneficiary", Col::Text),
    ("beneficiary_account", Col::Text),
    ("remittance_info", Col::Text),
    ("details_of_charges", Col::Text),
    ("sender_charges", Col::Money),
    ("sender_charges_currency", Col::Text),
    ("receiver_charges", Col::Money),
    ("receiver_charges_currency", Col::Text),
    ("sender_to_receiver_info", Col::Text),
    ("regulatory_reporting", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt103, Mt103Init, Mt103Stream<Source>, Mt103Row,
    name = "read_mt103",
    columns = MT103_COLUMNS,
    write = |output, batch| {
        write_date(output, 12, &batch, |r: &Mt103Row| r.value_date);
        write_decimal(output, 14, &batch, |r: &Mt103Row| r.amount);
        write_decimal(output, 16, &batch, |r: &Mt103Row| r.instructed_amount);
        write_decimal(output, 36, &batch, |r: &Mt103Row| r.sender_charges);
        write_decimal(output, 38, &batch, |r: &Mt103Row| r.receiver_charges);
        write_text(output, 0, &batch, |r: &Mt103Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt103Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt103Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt103Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt103Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt103Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt103Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt103Row| &r.tx_ref);
        write_text(output, 8, &batch, |r: &Mt103Row| &r.time_indications);
        write_text(output, 9, &batch, |r: &Mt103Row| &r.bank_operation_code);
        write_text(output, 10, &batch, |r: &Mt103Row| &r.instruction_codes);
        write_text(output, 11, &batch, |r: &Mt103Row| &r.transaction_type_code);
        write_text(output, 13, &batch, |r: &Mt103Row| &r.currency);
        write_text(output, 15, &batch, |r: &Mt103Row| &r.instructed_currency);
        write_text(output, 17, &batch, |r: &Mt103Row| &r.exchange_rate);
        write_text(output, 18, &batch, |r: &Mt103Row| &r.party_option_50);
        write_text(output, 19, &batch, |r: &Mt103Row| &r.ordering_customer);
        write_text(output, 20, &batch, |r: &Mt103Row| &r.ordering_customer_account);
        write_text(output, 21, &batch, |r: &Mt103Row| &r.sending_institution);
        write_text(output, 22, &batch, |r: &Mt103Row| &r.ordering_institution);
        write_text(output, 23, &batch, |r: &Mt103Row| &r.ordering_institution_account);
        write_text(output, 24, &batch, |r: &Mt103Row| &r.senders_correspondent);
        write_text(output, 25, &batch, |r: &Mt103Row| &r.senders_correspondent_account);
        write_text(output, 26, &batch, |r: &Mt103Row| &r.receivers_correspondent);
        write_text(output, 27, &batch, |r: &Mt103Row| &r.third_reimbursement_institution);
        write_text(output, 28, &batch, |r: &Mt103Row| &r.intermediary_institution);
        write_text(output, 29, &batch, |r: &Mt103Row| &r.account_with_institution);
        write_text(output, 30, &batch, |r: &Mt103Row| &r.account_with_institution_account);
        write_text(output, 31, &batch, |r: &Mt103Row| &r.party_option_59);
        write_text(output, 32, &batch, |r: &Mt103Row| &r.beneficiary);
        write_text(output, 33, &batch, |r: &Mt103Row| &r.beneficiary_account);
        write_text(output, 34, &batch, |r: &Mt103Row| &r.remittance_info);
        write_text(output, 35, &batch, |r: &Mt103Row| &r.details_of_charges);
        write_text(output, 37, &batch, |r: &Mt103Row| &r.sender_charges_currency);
        write_text(output, 39, &batch, |r: &Mt103Row| &r.receiver_charges_currency);
        write_text(output, 40, &batch, |r: &Mt103Row| &r.sender_to_receiver_info);
        write_text(output, 41, &batch, |r: &Mt103Row| &r.regulatory_reporting);
        write_text(output, 42, &batch, |r: &Mt103Row| &r.source_file);
    }
}

// ── read_mt202 ──────────────────────────────────────────────────────────────

const MT202_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    // COV when the user header says {119:COV}, else NULL. The wire type is 202
    // either way, so this column is the only thing that says it is a cover.
    ("variant", Col::Text),
    ("tx_ref", Col::Text),
    ("related_ref", Col::Text),
    ("time_indications", Col::Text),
    ("value_date", Col::Date),
    ("currency", Col::Text),
    ("amount", Col::Money),
    ("ordering_institution", Col::Text),
    ("ordering_institution_account", Col::Text),
    ("senders_correspondent", Col::Text),
    ("senders_correspondent_account", Col::Text),
    ("receivers_correspondent", Col::Text),
    ("intermediary_institution", Col::Text),
    ("account_with_institution", Col::Text),
    ("account_with_institution_account", Col::Text),
    ("beneficiary_institution", Col::Text),
    ("beneficiary_institution_account", Col::Text),
    ("sender_to_receiver_info", Col::Text),
    // Sequence B: the underlying customer transfer a cover carries. NULL on a
    // plain MT202, which has no sequence B at all.
    ("cov_ordering_customer", Col::Text),
    ("cov_ordering_customer_account", Col::Text),
    ("cov_ordering_institution", Col::Text),
    ("cov_intermediary_institution", Col::Text),
    ("cov_account_with_institution", Col::Text),
    ("cov_beneficiary", Col::Text),
    ("cov_beneficiary_account", Col::Text),
    ("cov_remittance_info", Col::Text),
    ("cov_sender_to_receiver_info", Col::Text),
    ("cov_instructed_currency", Col::Text),
    ("cov_instructed_amount", Col::Money),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt202, Mt202Init, Mt202Stream<Source>, Mt202Row,
    name = "read_mt202",
    columns = MT202_COLUMNS,
    write = |output, batch| {
        write_date(output, 11, &batch, |r: &Mt202Row| r.value_date);
        write_decimal(output, 13, &batch, |r: &Mt202Row| r.amount);
        write_decimal(output, 35, &batch, |r: &Mt202Row| r.cov_instructed_amount);
        write_text(output, 0, &batch, |r: &Mt202Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt202Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt202Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt202Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt202Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt202Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt202Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt202Row| &r.variant);
        write_text(output, 8, &batch, |r: &Mt202Row| &r.tx_ref);
        write_text(output, 9, &batch, |r: &Mt202Row| &r.related_ref);
        write_text(output, 10, &batch, |r: &Mt202Row| &r.time_indications);
        write_text(output, 12, &batch, |r: &Mt202Row| &r.currency);
        write_text(output, 14, &batch, |r: &Mt202Row| &r.ordering_institution);
        write_text(output, 15, &batch, |r: &Mt202Row| &r.ordering_institution_account);
        write_text(output, 16, &batch, |r: &Mt202Row| &r.senders_correspondent);
        write_text(output, 17, &batch, |r: &Mt202Row| &r.senders_correspondent_account);
        write_text(output, 18, &batch, |r: &Mt202Row| &r.receivers_correspondent);
        write_text(output, 19, &batch, |r: &Mt202Row| &r.intermediary_institution);
        write_text(output, 20, &batch, |r: &Mt202Row| &r.account_with_institution);
        write_text(output, 21, &batch, |r: &Mt202Row| &r.account_with_institution_account);
        write_text(output, 22, &batch, |r: &Mt202Row| &r.beneficiary_institution);
        write_text(output, 23, &batch, |r: &Mt202Row| &r.beneficiary_institution_account);
        write_text(output, 24, &batch, |r: &Mt202Row| &r.sender_to_receiver_info);
        write_text(output, 25, &batch, |r: &Mt202Row| &r.cov_ordering_customer);
        write_text(output, 26, &batch, |r: &Mt202Row| &r.cov_ordering_customer_account);
        write_text(output, 27, &batch, |r: &Mt202Row| &r.cov_ordering_institution);
        write_text(output, 28, &batch, |r: &Mt202Row| &r.cov_intermediary_institution);
        write_text(output, 29, &batch, |r: &Mt202Row| &r.cov_account_with_institution);
        write_text(output, 30, &batch, |r: &Mt202Row| &r.cov_beneficiary);
        write_text(output, 31, &batch, |r: &Mt202Row| &r.cov_beneficiary_account);
        write_text(output, 32, &batch, |r: &Mt202Row| &r.cov_remittance_info);
        write_text(output, 33, &batch, |r: &Mt202Row| &r.cov_sender_to_receiver_info);
        write_text(output, 34, &batch, |r: &Mt202Row| &r.cov_instructed_currency);
        write_text(output, 36, &batch, |r: &Mt202Row| &r.source_file);
    }
}

// ── read_mt940 ──────────────────────────────────────────────────────────────

const MT940_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    ("tx_ref", Col::Text),
    ("related_ref", Col::Text),
    ("account", Col::Text),
    ("account_bic", Col::Text),
    ("statement_number", Col::Int),
    ("sequence_number", Col::Int),
    // F is the first balance of a statement, M an intermediate one: a statement
    // split over several messages opens on M for every page but the first.
    ("opening_balance_kind", Col::Text),
    ("opening_balance_dc", Col::Text),
    ("opening_balance_date", Col::Date),
    ("opening_balance_currency", Col::Text),
    ("opening_balance", Col::Money),
    ("closing_balance_kind", Col::Text),
    ("closing_balance_dc", Col::Text),
    ("closing_balance_date", Col::Date),
    ("closing_balance_currency", Col::Text),
    ("closing_balance", Col::Money),
    ("available_balance_dc", Col::Text),
    ("available_balance_date", Col::Date),
    ("available_balance_currency", Col::Text),
    ("available_balance", Col::Money),
    ("forward_available_dc", Col::Text),
    ("forward_available_date", Col::Date),
    ("forward_available_currency", Col::Text),
    ("forward_available", Col::Money),
    // 1-based within the statement. NULL on the one row a statement with no
    // :61: line still yields, which carries its balances and nothing else.
    ("entry_index", Col::Int),
    ("value_date", Col::Date),
    ("entry_date", Col::Date),
    ("credit_debit", Col::Text),
    ("funds_code", Col::Text),
    ("amount", Col::Money),
    ("transaction_type", Col::Text),
    ("transaction_code", Col::Text),
    ("customer_ref", Col::Text),
    ("bank_ref", Col::Text),
    ("supplementary_details", Col::Text),
    ("narrative", Col::Text),
    // A :86: after the closing balance describes the statement, not an entry.
    ("statement_narrative", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt940, Mt940Init, Mt940Stream<Source>, Mt940Row,
    name = "read_mt940",
    columns = MT940_COLUMNS,
    write = |output, batch| {
        write_date(output, 15, &batch, |r: &Mt940Row| r.opening_balance_date);
        write_date(output, 20, &batch, |r: &Mt940Row| r.closing_balance_date);
        write_date(output, 24, &batch, |r: &Mt940Row| r.available_balance_date);
        write_date(output, 28, &batch, |r: &Mt940Row| r.forward_available_date);
        write_date(output, 32, &batch, |r: &Mt940Row| r.value_date);
        write_date(output, 33, &batch, |r: &Mt940Row| r.entry_date);
        write_decimal(output, 17, &batch, |r: &Mt940Row| r.opening_balance);
        write_decimal(output, 22, &batch, |r: &Mt940Row| r.closing_balance);
        write_decimal(output, 26, &batch, |r: &Mt940Row| r.available_balance);
        write_decimal(output, 30, &batch, |r: &Mt940Row| r.forward_available);
        write_decimal(output, 36, &batch, |r: &Mt940Row| r.amount);
        write_bigint(output, 11, &batch, |r: &Mt940Row| r.statement_number);
        write_bigint(output, 12, &batch, |r: &Mt940Row| r.sequence_number);
        write_bigint(output, 31, &batch, |r: &Mt940Row| r.entry_index);
        write_text(output, 0, &batch, |r: &Mt940Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt940Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt940Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt940Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt940Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt940Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt940Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt940Row| &r.tx_ref);
        write_text(output, 8, &batch, |r: &Mt940Row| &r.related_ref);
        write_text(output, 9, &batch, |r: &Mt940Row| &r.account);
        write_text(output, 10, &batch, |r: &Mt940Row| &r.account_bic);
        write_text(output, 13, &batch, |r: &Mt940Row| &r.opening_balance_kind);
        write_text(output, 14, &batch, |r: &Mt940Row| &r.opening_balance_dc);
        write_text(output, 16, &batch, |r: &Mt940Row| &r.opening_balance_currency);
        write_text(output, 18, &batch, |r: &Mt940Row| &r.closing_balance_kind);
        write_text(output, 19, &batch, |r: &Mt940Row| &r.closing_balance_dc);
        write_text(output, 21, &batch, |r: &Mt940Row| &r.closing_balance_currency);
        write_text(output, 23, &batch, |r: &Mt940Row| &r.available_balance_dc);
        write_text(output, 25, &batch, |r: &Mt940Row| &r.available_balance_currency);
        write_text(output, 27, &batch, |r: &Mt940Row| &r.forward_available_dc);
        write_text(output, 29, &batch, |r: &Mt940Row| &r.forward_available_currency);
        write_text(output, 34, &batch, |r: &Mt940Row| &r.credit_debit);
        write_text(output, 35, &batch, |r: &Mt940Row| &r.funds_code);
        write_text(output, 37, &batch, |r: &Mt940Row| &r.transaction_type);
        write_text(output, 38, &batch, |r: &Mt940Row| &r.transaction_code);
        write_text(output, 39, &batch, |r: &Mt940Row| &r.customer_ref);
        write_text(output, 40, &batch, |r: &Mt940Row| &r.bank_ref);
        write_text(output, 41, &batch, |r: &Mt940Row| &r.supplementary_details);
        write_text(output, 42, &batch, |r: &Mt940Row| &r.narrative);
        write_text(output, 43, &batch, |r: &Mt940Row| &r.statement_narrative);
        write_text(output, 44, &batch, |r: &Mt940Row| &r.source_file);
    }
}

// ── read_mt942 ──────────────────────────────────────────────────────────────

const MT942_COLUMNS: &[(&str, Col)] = &[
    ("direction", Col::Text),
    ("message_type", Col::Text),
    ("sender_bic", Col::Text),
    ("receiver_bic", Col::Text),
    ("uetr", Col::Text),
    ("validation_flag", Col::Text),
    ("mur", Col::Text),
    ("tx_ref", Col::Text),
    ("related_ref", Col::Text),
    ("account", Col::Text),
    ("account_bic", Col::Text),
    ("statement_number", Col::Int),
    ("sequence_number", Col::Int),
    // :34F: reports the threshold below which entries were left out. One
    // occurrence with no D/C mark applies to both sides and fills both pairs.
    ("floor_limit_debit", Col::Money),
    ("floor_limit_debit_currency", Col::Text),
    ("floor_limit_credit", Col::Money),
    ("floor_limit_credit_currency", Col::Text),
    // As the bank wrote it, with the offset beside it rather than folded in:
    // rewriting it to UTC loses which day the bank meant.
    ("report_datetime", Col::Stamp),
    ("report_utc_offset", Col::Text),
    ("entry_index", Col::Int),
    ("value_date", Col::Date),
    ("entry_date", Col::Date),
    ("credit_debit", Col::Text),
    ("funds_code", Col::Text),
    ("amount", Col::Money),
    ("transaction_type", Col::Text),
    ("transaction_code", Col::Text),
    ("customer_ref", Col::Text),
    ("bank_ref", Col::Text),
    ("supplementary_details", Col::Text),
    ("narrative", Col::Text),
    ("statement_narrative", Col::Text),
    ("debit_entry_count", Col::Int),
    ("debit_entry_currency", Col::Text),
    ("debit_entry_sum", Col::Money),
    ("credit_entry_count", Col::Int),
    ("credit_entry_currency", Col::Text),
    ("credit_entry_sum", Col::Money),
    ("source_file", Col::Text),
];

table_function! {
    ReadMt942, Mt942Init, Mt942Stream<Source>, Mt942Row,
    name = "read_mt942",
    columns = MT942_COLUMNS,
    write = |output, batch| {
        write_date(output, 20, &batch, |r: &Mt942Row| r.value_date);
        write_date(output, 21, &batch, |r: &Mt942Row| r.entry_date);
        write_timestamp(output, 17, &batch, |r: &Mt942Row| r.report_datetime);
        write_decimal(output, 13, &batch, |r: &Mt942Row| r.floor_limit_debit);
        write_decimal(output, 15, &batch, |r: &Mt942Row| r.floor_limit_credit);
        write_decimal(output, 24, &batch, |r: &Mt942Row| r.amount);
        write_decimal(output, 34, &batch, |r: &Mt942Row| r.debit_entry_sum);
        write_decimal(output, 37, &batch, |r: &Mt942Row| r.credit_entry_sum);
        write_bigint(output, 11, &batch, |r: &Mt942Row| r.statement_number);
        write_bigint(output, 12, &batch, |r: &Mt942Row| r.sequence_number);
        write_bigint(output, 19, &batch, |r: &Mt942Row| r.entry_index);
        write_bigint(output, 32, &batch, |r: &Mt942Row| r.debit_entry_count);
        write_bigint(output, 35, &batch, |r: &Mt942Row| r.credit_entry_count);
        write_text(output, 0, &batch, |r: &Mt942Row| &r.direction);
        write_text(output, 1, &batch, |r: &Mt942Row| &r.message_type);
        write_text(output, 2, &batch, |r: &Mt942Row| &r.sender_bic);
        write_text(output, 3, &batch, |r: &Mt942Row| &r.receiver_bic);
        write_text(output, 4, &batch, |r: &Mt942Row| &r.uetr);
        write_text(output, 5, &batch, |r: &Mt942Row| &r.validation_flag);
        write_text(output, 6, &batch, |r: &Mt942Row| &r.mur);
        write_text(output, 7, &batch, |r: &Mt942Row| &r.tx_ref);
        write_text(output, 8, &batch, |r: &Mt942Row| &r.related_ref);
        write_text(output, 9, &batch, |r: &Mt942Row| &r.account);
        write_text(output, 10, &batch, |r: &Mt942Row| &r.account_bic);
        write_text(output, 14, &batch, |r: &Mt942Row| &r.floor_limit_debit_currency);
        write_text(output, 16, &batch, |r: &Mt942Row| &r.floor_limit_credit_currency);
        write_text(output, 18, &batch, |r: &Mt942Row| &r.report_utc_offset);
        write_text(output, 22, &batch, |r: &Mt942Row| &r.credit_debit);
        write_text(output, 23, &batch, |r: &Mt942Row| &r.funds_code);
        write_text(output, 25, &batch, |r: &Mt942Row| &r.transaction_type);
        write_text(output, 26, &batch, |r: &Mt942Row| &r.transaction_code);
        write_text(output, 27, &batch, |r: &Mt942Row| &r.customer_ref);
        write_text(output, 28, &batch, |r: &Mt942Row| &r.bank_ref);
        write_text(output, 29, &batch, |r: &Mt942Row| &r.supplementary_details);
        write_text(output, 30, &batch, |r: &Mt942Row| &r.narrative);
        write_text(output, 31, &batch, |r: &Mt942Row| &r.statement_narrative);
        write_text(output, 33, &batch, |r: &Mt942Row| &r.debit_entry_currency);
        write_text(output, 36, &batch, |r: &Mt942Row| &r.credit_entry_currency);
        write_text(output, 38, &batch, |r: &Mt942Row| &r.source_file);
    }
}

// ── sniff_iso20022 ───────────────────────────────────────────────────────────

const SNIFF_COLUMNS: &[(&str, Col)] = &[
    ("message_type", Col::Text),
    ("family", Col::Text),
    ("namespace", Col::Text),
    ("msg_id", Col::Text),
    ("created", Col::Stamp),
    ("records", Col::Int),
    ("reader", Col::Text),
    ("error", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    SniffIso20022, SniffInit, SniffStream<Source>, SniffRow,
    name = "sniff_iso20022",
    columns = SNIFF_COLUMNS,
    write = |output, batch| {
        write_timestamp(output, 4, &batch, |r: &SniffRow| {
            r.created.as_deref().and_then(temporal::ts_micros)
        });
        write_bigint(output, 5, &batch, |r: &SniffRow| r.records);
        write_text(output, 0, &batch, |r: &SniffRow| &r.message_type);
        write_text(output, 1, &batch, |r: &SniffRow| &r.family);
        write_text(output, 2, &batch, |r: &SniffRow| &r.namespace);
        write_text(output, 3, &batch, |r: &SniffRow| &r.msg_id);
        write_text(output, 6, &batch, |r: &SniffRow| &r.reader);
        write_text(output, 7, &batch, |r: &SniffRow| &r.error);
        write_text(output, 8, &batch, |r: &SniffRow| &r.source_file);
    }
}

// ── audit_addresses ──────────────────────────────────────────────────────────

const AUDIT_ADDRESSES_COLUMNS: &[(&str, Col)] = &[
    ("family", Col::Text),
    ("message_id", Col::Text),
    // NULL for a party stated once for the message or the payment group rather
    // than inside a transaction.
    ("record_index", Col::Int),
    ("party_path", Col::Text),
    ("role", Col::Text),
    ("party_kind", Col::Text),
    ("name", Col::Text),
    ("bic", Col::Text),
    ("town", Col::Text),
    ("country", Col::Text),
    // What the counts beside it do not give: the lines themselves, newline-joined,
    // so a refusal can be acted on without going back to the file.
    ("address_text", Col::Text),
    ("address_lines", Col::Int),
    ("longest_address_line", Col::Int),
    ("structured_elements", Col::Int),
    // The four shapes off the wire; the verdict is `finding`.
    ("address_format", Col::Text),
    ("finding", Col::Text),
    ("source_file", Col::Text),
];

table_function! {
    AuditAddresses, AuditAddressesInit, Addresses<Source>, AddrRow,
    name = "audit_addresses",
    columns = AUDIT_ADDRESSES_COLUMNS,
    write = |output, batch| {
        write_bigint(output, 2, &batch, |r: &AddrRow| r.record_index);
        write_bigint(output, 11, &batch, |r: &AddrRow| r.address_lines);
        write_bigint(output, 12, &batch, |r: &AddrRow| r.longest_address_line);
        write_bigint(output, 13, &batch, |r: &AddrRow| r.structured_elements);
        write_text(output, 0, &batch, |r: &AddrRow| &r.family);
        write_text(output, 1, &batch, |r: &AddrRow| &r.message_id);
        write_text(output, 3, &batch, |r: &AddrRow| &r.party_path);
        write_text(output, 4, &batch, |r: &AddrRow| &r.role);
        write_text(output, 5, &batch, |r: &AddrRow| &r.party_kind);
        write_text(output, 6, &batch, |r: &AddrRow| &r.name);
        write_text(output, 7, &batch, |r: &AddrRow| &r.bic);
        write_text(output, 8, &batch, |r: &AddrRow| &r.town);
        write_text(output, 9, &batch, |r: &AddrRow| &r.country);
        write_text(output, 10, &batch, |r: &AddrRow| &r.address_text);
        write_text(output, 14, &batch, |r: &AddrRow| &r.address_format);
        write_text(output, 15, &batch, |r: &AddrRow| &r.finding);
        write_text(output, 16, &batch, |r: &AddrRow| &r.source_file);
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<ReadIso20022>("read_iso20022")?;
    con.register_table_function::<ReadCamtTransactions>("read_camt_transactions")?;
    con.register_table_function::<ReadCamtBalances>("read_camt_balances")?;
    con.register_table_function::<ReadCamtAmountDetails>("read_camt_amount_details")?;
    con.register_table_function::<ReadCamtRemittance>("read_camt_remittance")?;
    con.register_table_function::<ReadPacs008>("read_pacs008")?;
    con.register_table_function::<ReadPacs004>("read_pacs004")?;
    con.register_table_function::<ReadPacs002>("read_pacs002")?;
    con.register_table_function::<ReadPacs028>("read_pacs028")?;
    con.register_table_function::<ReadPain001>("read_pain001")?;
    con.register_table_function::<ReadPain002>("read_pain002")?;
    con.register_table_function::<ReadPain008>("read_pain008")?;
    con.register_table_function::<ReadPain009>("read_pain009")?;
    con.register_table_function::<ReadPain010>("read_pain010")?;
    con.register_table_function::<ReadPain011>("read_pain011")?;
    con.register_table_function::<ReadPain012>("read_pain012")?;
    con.register_table_function::<ReadPain013>("read_pain013")?;
    con.register_table_function::<ReadPain014>("read_pain014")?;
    con.register_table_function::<ReadCamt056>("read_camt056")?;
    con.register_table_function::<ReadCamt057>("read_camt057")?;
    con.register_table_function::<ReadPacs009>("read_pacs009")?;
    con.register_table_function::<ReadPacs010>("read_pacs010")?;
    con.register_table_function::<ReadPacs003>("read_pacs003")?;
    con.register_table_function::<ReadPacs007>("read_pacs007")?;
    con.register_table_function::<ReadCamt055>("read_camt055")?;
    con.register_table_function::<ReadCamt029>("read_camt029")?;
    con.register_table_function::<ReadCamt027>("read_camt027")?;
    con.register_table_function::<ReadCamt028>("read_camt028")?;
    con.register_table_function::<ReadCamt030>("read_camt030")?;
    con.register_table_function::<ReadCamt031>("read_camt031")?;
    con.register_table_function::<ReadCamt036>("read_camt036")?;
    con.register_table_function::<ReadCamt037>("read_camt037")?;
    con.register_table_function::<ReadCamt087>("read_camt087")?;
    con.register_table_function::<ReadMt101>("read_mt101")?;
    con.register_table_function::<ReadMt104>("read_mt104")?;
    con.register_table_function::<ReadMt103>("read_mt103")?;
    con.register_table_function::<ReadMt202>("read_mt202")?;
    con.register_table_function::<ReadMt940>("read_mt940")?;
    con.register_table_function::<ReadMt942>("read_mt942")?;
    con.register_table_function::<SniffIso20022>("sniff_iso20022")?;
    con.register_table_function::<AuditAddresses>("audit_addresses")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const SAMPLE: &str = "testdata/camt053_sample.xml";

    /// Every entry a scan produced, as the pair a caller would notice if the
    /// bytes had arrived any differently.
    fn rows(path: &Path) -> Vec<(String, i128)> {
        rows_of(&[path.to_string_lossy().into_owned()])
    }

    /// Every reader here numbers its output columns by hand, once per column, in
    /// a `write` block of up to forty lines. `flat_vector` does not check the
    /// number: past the last column it hands back whatever is at that offset, and
    /// an off-by-one writes through it. This is what turns that into a panic --
    /// found by making the mistake in `read_mt101`, where writing column 39 of a
    /// 39-column chunk corrupted a decimal eleven columns away and every test
    /// still passed.
    #[test]
    #[should_panic(expected = "column 39 written on a chunk of 39 columns")]
    fn a_column_index_past_the_last_column_panics_rather_than_corrupting_one() {
        in_range(39, 39);
    }

    fn rows_of(files: &[String]) -> Vec<(String, i128)> {
        let mut state = ScanState::<EntryStream<Source>>::new();
        let mut out = Vec::new();
        loop {
            let batch = pull_batch::<EntryStream<Source>>(files, &mut state, "read_iso20022")
                .expect("the sample parses");
            if batch.is_empty() {
                return out;
            }
            out.extend(batch.iter().map(|row| {
                (
                    row.entry_ref.clone().unwrap_or_default(),
                    row.amount.unwrap_or_default(),
                )
            }));
        }
    }

    /// One gzip member per chunk, concatenated. `cat a.xml.gz b.xml.gz` and a
    /// dump appended over a day both look like this on disk.
    fn gzipped(members: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for member in members {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(member).expect("gzip a member");
            out.extend(enc.finish().expect("finish a member"));
        }
        out
    }

    const XML_PREFIX_BYTES: usize = 64 * 1024;
    const NO_MARKUP_ERROR: &str = "not XML: no markup in the first 64 KiB";
    const MT_BEFORE_MARKUP_ERROR: &str = "not XML: SWIFT MT marker before markup";

    fn reader_error<S: RowStream>(path: &Path, fname: &str) -> String {
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<S>::new();
        match pull_batch::<S>(&files, &mut state, fname) {
            Ok(rows) => panic!(
                "{fname} accepted {} row(s) from {}",
                rows.len(),
                path.display()
            ),
            Err(e) => e.to_string(),
        }
    }

    fn read_iso_error(path: &Path) -> String {
        reader_error::<EntryStream<Source>>(path, "read_iso20022")
    }

    fn assert_no_markup_error(name: &str, err: &str) {
        assert!(
            err.ends_with(NO_MARKUP_ERROR),
            "{name}: expected {NO_MARKUP_ERROR:?}, got {err}"
        );
    }

    fn written(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("quackiso-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("temp fixture is writable");
        path
    }

    /// A glob matches directories as readily as files, and `read_iso20022` used
    /// to hand one straight to `File::open`. The other half of the predicate --
    /// that a FIFO survives it -- is `a_statement_may_arrive_down_a_pipe`, which
    /// only Unix can run, so this holds the directory side everywhere.
    #[test]
    fn a_glob_yields_files_and_skips_the_directories_it_matched() {
        let dir = std::env::temp_dir().join(format!("quackiso-{}-inbox", std::process::id()));
        // a failing run never reaches the cleanup below, and the leftover would
        // fail the next one for the wrong reason
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("archive")).expect("temp inbox is writable");
        let file = dir.join("stmt.xml");
        std::fs::write(&file, b"<Document/>").expect("temp fixture is writable");

        let got = resolve_files(&format!("{}/*", dir.display()), "read_iso20022")
            .expect("the directory must not make this fail");
        assert_eq!(got.len(), 1, "only the file is a scan input: {got:?}");
        assert!(got[0].ends_with("stmt.xml"), "{got:?}");

        // and a directory named outright is still not something to parse
        let named = resolve_files(&dir.join("archive").display().to_string(), "read_iso20022");
        assert!(named.is_err(), "a directory is not a statement");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gzip_reads_exactly_like_the_plain_file() {
        let want = rows(Path::new(SAMPLE));
        assert_eq!(want.len(), 2, "the sample holds two entries");

        let plain = std::fs::read(SAMPLE).expect("the sample is readable");
        let (head, tail) = plain.split_at(plain.len() / 2);
        let cases = [
            // the ordinary case: one member
            ("single.xml.gz", gzipped(&[&plain])),
            // two members split mid-document: decoded, they are one file again
            ("multi.xml.gz", gzipped(&[head, tail])),
            // detection is by content, so the name is allowed to lie
            ("misnamed.xml", gzipped(&[&plain])),
        ];
        for (name, bytes) in cases {
            let path = written(name, &bytes);
            assert_eq!(rows(&path), want, "{name}");
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn a_broken_gzip_fails_instead_of_panicking() {
        let plain = std::fs::read(SAMPLE).expect("the sample is readable");
        let whole = gzipped(&[&plain]);
        let cases = [
            // cut mid-stream: the decoder runs out of input mid-document
            ("truncated.xml.gz", whole[..whole.len() / 2].to_vec()),
            // the magic is there and the deflate data behind it is not
            (
                "garbage.xml.gz",
                vec![0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 3, 9, 9, 9, 9],
            ),
            // shorter than the magic itself: read as XML, and it is not XML
            ("stub.xml", vec![0x1f]),
            // nothing at all
            ("empty.xml", Vec::new()),
            // a whole member and then bytes that are not a member
            (
                "trailing.xml.gz",
                [whole.clone(), b"not a member".to_vec()].concat(),
            ),
            // zero padding, which block-oriented writers leave behind
            ("padded.xml.gz", [whole.clone(), vec![0; 8]].concat()),
            // gzip of a gzip: one layer off, and what is inside is not XML
            ("double.xml.gz", gzipped(&[&whole])),
        ];
        for (name, bytes) in cases {
            let path = written(name, &bytes);
            let files = vec![path.to_string_lossy().into_owned()];
            let mut state = ScanState::<EntryStream<Source>>::new();
            let got = pull_batch::<EntryStream<Source>>(&files, &mut state, "read_iso20022");
            let err = got
                .err()
                .unwrap_or_else(|| panic!("{name} must fail loudly"));
            if name != "double.xml.gz" {
                assert!(
                    err.to_string().contains(name),
                    "{name}: the error does not name the file: {err}"
                );
            }
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn xml_reader_rejects_markup_free() {
        let plain = vec![b'a'; XML_PREFIX_BYTES * 2];
        let (head, tail) = plain.split_at(XML_PREFIX_BYTES);
        for (name, bytes) in [
            ("plain-no-markup.txt", plain.clone()),
            ("gzip-no-markup.txt.gz", gzipped(&[&plain])),
            ("gzip-no-markup-concat.txt.gz", gzipped(&[head, tail])),
        ] {
            let path = written(name, &bytes);
            let err = read_iso_error(&path);
            assert_no_markup_error(name, &err);
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn xml_reader_rejects_markup_after_prefix() {
        let mut body = vec![b'a'; XML_PREFIX_BYTES];
        body.extend_from_slice(b"<Document/>");
        let (head, tail) = body.split_at(XML_PREFIX_BYTES);
        for (name, bytes) in [
            ("markup-after-prefix.xml", body.clone()),
            ("markup-after-prefix.xml.gz", gzipped(&[&body])),
            ("markup-after-prefix-concat.xml.gz", gzipped(&[head, tail])),
        ] {
            let path = written(name, &bytes);
            let err = read_iso_error(&path);
            assert_no_markup_error(name, &err);
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn xml_reader_rejects_mt_marker_before_markup() {
        let mut body = b"{1:F01BANKBEBBAXXX0000000000}{4:\n:20:REF\n-}".to_vec();
        body.extend_from_slice(b"<Document/>");
        for (name, bytes) in [
            ("mt-marker-before-markup.txt", body.clone()),
            ("mt-marker-before-markup.txt.gz", gzipped(&[&body])),
        ] {
            let path = written(name, &bytes);
            let err = read_iso_error(&path);
            assert!(
                err.ends_with(MT_BEFORE_MARKUP_ERROR),
                "{name}: expected {MT_BEFORE_MARKUP_ERROR:?}, got {err}"
            );
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn markup_inside_prefix_still_reaches_parser() {
        let path = written("markup-inside-prefix.xml", b"<Document><Stmt>");
        let err = read_iso_error(&path);
        assert!(
            err.contains("not well-formed XML: end of input inside <Stmt>"),
            "markup inside the prefix should reach quick-xml, got {err}"
        );
        assert!(
            !err.ends_with(NO_MARKUP_ERROR),
            "markup inside the prefix must not be classified as absent markup"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mt_reader_keeps_markup_free_framing() {
        let got =
            count::<Mt940Stream<Source>>(Path::new("testdata/mt940_statement.txt"), "read_mt940");
        assert!(
            got > 0,
            "the MT fixture should still parse without XML markup"
        );
    }

    /// A tar of `members`, headers checksummed the way tar(1) writes them.
    /// This is the archive the refusal exists for: parsed as one document it
    /// used to return the entries of both statements under one `source_file`,
    /// with nothing on the row saying they came from two.
    fn tar_of(members: &[(&str, &[u8])]) -> Vec<u8> {
        const BLOCK: usize = 512;
        let mut out = Vec::new();
        for (name, body) in members {
            let mut header = vec![0u8; BLOCK];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"000644 \0");
            header[108..116].copy_from_slice(b"000000 \0");
            header[116..124].copy_from_slice(b"000000 \0");
            header[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
            header[136..148].copy_from_slice(b"14657513614 ");
            header[156] = b'0';
            header[257..265].copy_from_slice(b"ustar  \0");
            let sum: u64 = header
                .iter()
                .enumerate()
                .map(|(at, byte)| match (148..156).contains(&at) {
                    true => u64::from(b' '),
                    false => u64::from(*byte),
                })
                .sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(body);
            out.extend(std::iter::repeat_n(
                0u8,
                (BLOCK - body.len() % BLOCK) % BLOCK,
            ));
        }
        // the two zero blocks that end an archive
        out.extend(std::iter::repeat_n(0u8, 2 * BLOCK));
        out
    }

    fn sniff_row(path: &Path) -> SniffRow {
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<SniffStream<Source>>::new();
        let mut rows = pull_batch::<SniffStream<Source>>(&files, &mut state, "sniff_iso20022")
            .expect("sniff returns an inventory row");
        assert_eq!(rows.len(), 1, "sniff emits one row per file");
        rows.remove(0)
    }

    fn assert_named(kind: ContainerKind, name: &str, err: &str) {
        assert!(
            err.ends_with(kind.reason()),
            "{name}: expected {:?}, got {err}",
            kind.reason()
        );
    }

    /// An archive is not a message, and every public path says so in the same
    /// sentence: the sniffer in its `error` column, the readers and the audit as
    /// a raise carrying the path. What none of them does is read a member.
    #[test]
    fn every_public_path_names_a_container_rather_than_reading_it() {
        let statement = std::fs::read(SAMPLE).expect("the fixture is readable");
        let tar = tar_of(&[("january.xml", &statement), ("february.xml", &statement)]);
        let path = written("two-statements.tar", &tar);

        assert_eq!(
            sniff_row(&path).error.as_deref(),
            Some(ContainerKind::Tar.reason()),
            "the sniffer names the archive"
        );
        for (label, err) in [
            ("read_iso20022", read_iso_error(&path)),
            (
                "read_mt940",
                reader_error::<Mt940Stream<Source>>(&path, "read_mt940"),
            ),
            (
                "audit_addresses",
                reader_error::<Addresses<Source>>(&path, "audit_addresses"),
            ),
        ] {
            assert_named(ContainerKind::Tar, label, &err);
            assert!(
                err.contains("two-statements.tar"),
                "{label}: the refusal should name the file, got {err}"
            );
        }
        std::fs::remove_file(&path).ok();
    }

    /// `Source` unwraps gzip before any shape is decided, so a `.tar.gz` is a
    /// TAR and not a mystery. The same property that makes a gzipped statement
    /// read like a plain one makes a gzipped archive refuse like a plain one.
    #[test]
    fn a_gzipped_archive_is_named_by_what_is_inside_it() {
        let statement = std::fs::read(SAMPLE).expect("the fixture is readable");
        let tar = tar_of(&[("january.xml", &statement)]);
        let path = written("statements.tar.gz", &gzipped(&[&tar]));
        assert_named(ContainerKind::Tar, "gzip", &read_iso_error(&path));
        assert_eq!(
            sniff_row(&path).error.as_deref(),
            Some(ContainerKind::Tar.reason())
        );
        std::fs::remove_file(&path).ok();
    }

    /// All five kinds through one shared guard. The detection itself is held in
    /// `container`; what this pins is that a reader speaks the same sentence for
    /// every one of them rather than for the archive it was written against.
    #[test]
    fn each_container_kind_reaches_the_reader_by_name() {
        let mut pgp = vec![0xC1, 0x0C, 0x03];
        pgp.extend_from_slice(&[0u8; 11]);
        pgp.extend_from_slice(&[0xD2, 0x20, 0x01]);
        pgp.extend_from_slice(&[0u8; 31]);
        for (kind, name, bytes) in [
            (
                ContainerKind::Zip,
                "delivery.zip",
                b"PK\x03\x04\x14\x00\x00\x00\x08\x00camt053.xml".to_vec(),
            ),
            (
                ContainerKind::Tar,
                "delivery.tar",
                tar_of(&[("camt053.xml", b"<Document/>")]),
            ),
            (
                ContainerKind::Pkcs7,
                "signed.p7m",
                b"-----BEGIN PKCS7-----\nMIIGpwYJKoZIhvcNAQcC\n".to_vec(),
            ),
            (ContainerKind::Pgp, "encrypted.pgp", pgp),
            (
                ContainerKind::Ebics,
                "request.xml",
                br#"<?xml version="1.0"?><ebicsRequest xmlns="urn:org:ebics:H005"><header authenticate="true"/></ebicsRequest>"#.to_vec(),
            ),
        ] {
            let path = written(name, &bytes);
            assert_named(kind, name, &read_iso_error(&path));
            assert_eq!(sniff_row(&path).error.as_deref(), Some(kind.reason()), "{name}");
            std::fs::remove_file(&path).ok();
        }
    }

    /// The MT guard subtracts nothing from the framer: it refuses a container
    /// and passes everything else through, including the markup-after-messages
    /// and bare-body shapes the corpus holds.
    #[test]
    fn the_mt_guard_refuses_only_a_container() {
        let tar = tar_of(&[("statement.sta", b":20:REF\n:61:2607290729D100,00NTRF//X\n-")]);
        let path = written("statements.tar", &tar);
        assert_named(
            ContainerKind::Tar,
            "read_mt940",
            &reader_error::<Mt940Stream<Source>>(&path, "read_mt940"),
        );
        std::fs::remove_file(&path).ok();

        for (fixture, fname, rows) in [
            ("testdata/mt940_statement.txt", "read_mt940", 0usize),
            ("testdata/mt103_customer_transfer.txt", "read_mt103", 0),
        ] {
            let got = match fname {
                "read_mt940" => count::<Mt940Stream<Source>>(Path::new(fixture), fname),
                _ => count::<Mt103Stream<Source>>(Path::new(fixture), fname),
            };
            assert!(got > rows, "{fixture}: the guard must not refuse it");
        }
    }

    /// A camt.053 around `body`, so an entity reference can be put at a
    /// group-level leaf, inside an entry subtree, or in a party name and read
    /// through whichever code path owns it.
    fn camt053_with(doctype: &str, msg_id: &str, remittance: &str, name: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{doctype}\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.08\">\
             <BkToCstmrStmt><GrpHdr><MsgId>{msg_id}</MsgId></GrpHdr>\
             <Stmt><Id>S-1</Id><Acct><Id><IBAN>CH9300762011623852957</IBAN></Id></Acct>\
             <Ntry><NtryRef>N-1</NtryRef><Amt Ccy=\"CHF\">1.00</Amt>\
             <CdtDbtInd>DBIT</CdtDbtInd><NtryDtls><TxDtls>\
             <RltdPties><Cdtr><Nm>{name}</Nm></Cdtr></RltdPties>\
             <RmtInf><Ustrd>{remittance}</Ustrd></RmtInf>\
             </TxDtls></NtryDtls></Ntry></Stmt></BkToCstmrStmt></Document>"
        )
    }

    const ENTITY_ERROR: &str = "unrecognized entity";

    /// An entity nothing declared is content this cannot read, and it is
    /// reported as such at every leaf rather than resolved, skipped, or handed
    /// over as the literal `&secret;`. The sniffer keeps its one row and says
    /// so in `error`; the readers and the audit raise.
    #[test]
    fn an_unknown_named_entity_is_a_content_error_and_never_a_value() {
        // the group-level leaf, read through `wire::event_text`
        let group = written(
            "entity-group.xml",
            camt053_with("", "&secret;", "Invoice 1", "ACME").as_bytes(),
        );
        let row = sniff_row(&group);
        assert!(
            row.error
                .as_deref()
                .is_some_and(|e| e.contains(ENTITY_ERROR)),
            "sniff: got {:?}",
            row.error
        );
        assert_eq!(row.msg_id, None, "the leaf never became a value");
        assert!(read_iso_error(&group).contains(ENTITY_ERROR));
        std::fs::remove_file(&group).ok();

        // and inside the entry subtree, read through serde
        let subtree = written(
            "entity-subtree.xml",
            camt053_with("", "M-1", "&secret;", "ACME").as_bytes(),
        );
        assert!(read_iso_error(&subtree).contains(ENTITY_ERROR));
        std::fs::remove_file(&subtree).ok();

        // and in a party name, read by the audit
        let party = written(
            "entity-party.xml",
            camt053_with("", "M-1", "Invoice 1", "&secret;").as_bytes(),
        );
        assert!(reader_error::<Addresses<Source>>(&party, "audit_addresses").contains(ENTITY_ERROR));
        std::fs::remove_file(&party).ok();
    }

    /// The XXE shape: a doctype declaring an entity, one of them pointing at a
    /// file on this disk. Nothing here processes a DTD, so both are unresolved
    /// entities and the sentinel never reaches a column. This is asserted
    /// rather than assumed, because "the parser does not do that" is exactly
    /// the kind of claim that stops being true on a dependency bump.
    #[test]
    fn a_declared_entity_is_not_expanded_and_its_target_never_appears() {
        const SENTINEL: &str = "QUACKISO-SENTINEL-9f2c";
        let secret = written("entity-secret.txt", SENTINEL.as_bytes());
        let doctype = format!(
            "<!DOCTYPE Document [<!ENTITY local \"{SENTINEL}\">\
             <!ENTITY remote SYSTEM \"file:///{}\">]>",
            secret.to_string_lossy().replace('\\', "/")
        );

        for (label, xml) in [
            (
                "internal",
                camt053_with(&doctype, "&local;", "Invoice 1", "ACME"),
            ),
            (
                "external",
                camt053_with(&doctype, "&remote;", "Invoice 1", "ACME"),
            ),
        ] {
            let path = written("entity-doctype.xml", xml.as_bytes());
            let row = sniff_row(&path);
            let error = row.error.clone().unwrap_or_default();
            assert!(error.contains(ENTITY_ERROR), "{label}: got {error}");
            let reported = format!("{row:?}");
            assert!(
                !reported.contains(SENTINEL),
                "{label}: the sentinel reached a column: {reported}"
            );
            let raised = read_iso_error(&path);
            assert!(raised.contains(ENTITY_ERROR), "{label}: got {raised}");
            assert!(!raised.contains(SENTINEL), "{label}: {raised}");
            std::fs::remove_file(&path).ok();
        }
        std::fs::remove_file(&secret).ok();
    }

    /// What entity handling still has to do. The five built-ins and a numeric
    /// reference are XML itself, not a DTD feature, and a refusal that took
    /// them with it would refuse `Smith &amp; Co`.
    #[test]
    fn a_built_in_or_numeric_entity_reference_is_still_read_as_text() {
        let path = written(
            "entity-builtin.xml",
            camt053_with(
                "",
                "M&amp;1",
                "Smith &amp; Co &#8212; invoice &#65;",
                "ACME",
            )
            .as_bytes(),
        );
        assert_eq!(sniff_row(&path).msg_id.as_deref(), Some("M&1"));
        let rows = rows_of(&[path.to_string_lossy().into_owned()]);
        assert_eq!(rows.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    /// The billion-laughs shape. Because no declaration is ever applied, the
    /// depth of the nesting changes nothing: the outermost reference is one
    /// unrecognised entity and the parse stops there. Two depths, the same
    /// refusal, and no replacement text built at either.
    #[test]
    fn nested_entity_declarations_do_not_expand_at_any_depth() {
        let mut errors = Vec::new();
        for depth in [3usize, 12] {
            let mut declarations = String::from("<!ENTITY e0 \"laugh\">");
            for level in 1..=depth {
                let previous = format!("&e{};", level - 1).repeat(10);
                declarations.push_str(&format!("<!ENTITY e{level} \"{previous}\">"));
            }
            let xml = camt053_with(
                &format!("<!DOCTYPE Document [{declarations}]>"),
                &format!("&e{depth};"),
                "Invoice 1",
                "ACME",
            );
            let path = written("entity-nested.xml", xml.as_bytes());
            let error = sniff_row(&path).error.unwrap_or_default();
            assert!(
                error.ends_with(&format!("{ENTITY_ERROR} `e{depth}`")),
                "depth {depth}: the outermost reference is what stopped it, got {error}"
            );
            assert!(
                !error.contains("laugh"),
                "depth {depth}: replacement text was built"
            );
            // Depth 12 declares 10^12 characters of replacement text. What the
            // refusal costs is the reference itself, so its length is what it
            // was at depth 3 plus the two digits of the name.
            errors.push(error.len());
            std::fs::remove_file(&path).ok();
        }
        assert!(
            errors[1] - errors[0] <= 1,
            "the refusal grew with the nesting: {errors:?}"
        );
    }

    #[test]
    fn sniff_stream_keeps_markup_free_input_as_error_row() {
        let bytes = vec![b'a'; XML_PREFIX_BYTES * 2];
        let path = written("sniff-no-markup.txt", &bytes);
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<SniffStream<Source>>::new();
        let rows = pull_batch::<SniffStream<Source>>(&files, &mut state, "sniff_iso20022")
            .expect("sniff returns an inventory row");
        assert_eq!(rows.len(), 1, "sniff emits one row per file");
        assert_eq!(rows[0].error.as_deref(), Some(NO_MARKUP_ERROR));
        assert!(rows[0].family.is_none());
        let tail = pull_batch::<SniffStream<Source>>(&files, &mut state, "sniff_iso20022")
            .expect("sniff scan drains");
        assert!(tail.is_empty(), "the one-file scan should be done");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sniff_stream_keeps_markup_after_prefix_as_error_row() {
        let mut bytes = vec![b'a'; XML_PREFIX_BYTES];
        bytes.extend_from_slice(b"<Document/>");
        let path = written("sniff-markup-after-prefix.txt", &bytes);
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<SniffStream<Source>>::new();
        let rows = pull_batch::<SniffStream<Source>>(&files, &mut state, "sniff_iso20022")
            .expect("sniff returns an inventory row");
        assert_eq!(rows.len(), 1, "sniff emits one row per file");
        assert_eq!(rows[0].error.as_deref(), Some(NO_MARKUP_ERROR));
        assert!(rows[0].family.is_none());
        std::fs::remove_file(&path).ok();
    }

    /// Compression lives in `Source`, which every reader shares, so a reader
    /// that is not `read_iso20022` gets it without knowing. pacs.008 stands in
    /// for the other thirty-two, and the prefixed fixture also puts namespace
    /// rewriting through the decoder.
    #[test]
    fn another_reader_gets_gzip_from_the_shared_source() {
        const PACS: &str = "testdata/pacs008_prefixed_sample.xml";
        let plain = std::fs::read(PACS).expect("the fixture is readable");
        let path = written("pacs008.xml.gz", &gzipped(&[&plain]));

        let want = count::<TxStream<Source>>(Path::new(PACS), "read_pacs008");
        assert!(want > 0, "the fixture must actually parse");
        assert_eq!(count::<TxStream<Source>>(&path, "read_pacs008"), want);
        std::fs::remove_file(&path).ok();
    }

    fn count<S: RowStream>(path: &Path, fname: &str) -> usize {
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<S>::new();
        let mut rows = 0;
        loop {
            let batch = pull_batch::<S>(&files, &mut state, fname).expect("the fixture parses");
            if batch.is_empty() {
                return rows;
            }
            rows += batch.len();
        }
    }

    /// The two grains a status request comes in. A request that names a whole
    /// original message and details no transaction is one GROUP row, not zero:
    /// "where is batch X?" has to be answerable in SQL.
    #[test]
    fn pacs028_streams_one_row_per_status_request() {
        let tx = count::<StsReqStream<Source>>(
            Path::new("testdata/pacs028_status_request.xml"),
            "read_pacs028",
        );
        assert_eq!(tx, 2, "two TxInf, two rows");
        let grp = count::<StsReqStream<Source>>(
            Path::new("testdata/pacs028_group_only.xml"),
            "read_pacs028",
        );
        assert_eq!(grp, 1, "a group-only request is still one row");
        // Both grains in one Document, transaction request first: the flag that
        // decides whether a closing container owes a GROUP row has to be
        // cleared at every container, or the second request is invisible.
        let mixed = count::<StsReqStream<Source>>(
            Path::new("testdata/pacs028_mixed_grains.xml"),
            "read_pacs028",
        );
        assert_eq!(mixed, 2, "one transaction row, then one group row");
    }

    /// The grain of the mandate family: the record element, not the message.
    /// Every one of the four repeats, so a file stating three mandates is
    /// three rows, and each reader answers only for its own container — the
    /// amendment and the cancellation both nest a `<Mndt>` that is not theirs
    /// to emit.
    #[test]
    fn the_mandate_readers_yield_one_row_per_record() {
        let cases: [(usize, &str); 4] = [
            (
                count::<MndtStream<Source>>(
                    Path::new("testdata/pain009_mandate.xml"),
                    "read_pain009",
                ),
                "pain.009",
            ),
            (
                count::<AmdmntStream<Source>>(
                    Path::new("testdata/pain010_amount_amendment.xml"),
                    "read_pain010",
                ),
                "pain.010",
            ),
            (
                count::<MndtCxlStream<Source>>(
                    Path::new("testdata/pain011_full_mandate.xml"),
                    "read_pain011",
                ),
                "pain.011",
            ),
            (
                count::<AccptncStream<Source>>(
                    Path::new("testdata/pain012_accepted.xml"),
                    "read_pain012",
                ),
                "pain.012",
            ),
        ];
        for (rows, family) in cases {
            assert_eq!(rows, 1, "{family}: one record, one row");
        }

        // pain.010 nests the original mandate inside the amendment and pain.009
        // reads `<Mndt>` as its record: pointed at an amendment, the initiation
        // reader must refuse the file rather than emit the two it can see.
        let files = vec!["testdata/pain010_amount_amendment.xml".to_string()];
        let mut state = ScanState::<MndtStream<Source>>::new();
        let err = pull_batch::<MndtStream<Source>>(&files, &mut state, "read_pain009")
            .expect_err("an amendment is not an initiation request");
        assert!(err.to_string().contains("no <MndtInitnReq> found"), "{err}");
    }

    /// The grain of the seven investigation readers: the message, not a record
    /// inside it. Nothing in `Assgnmt`, `Case` or `Undrlyg` repeats, and the row
    /// is emitted when the container closes — the payload follows the case in
    /// document order, so a reader emitting at the case would lose it.
    #[test]
    fn the_investigation_readers_yield_one_row_per_message() {
        let cases: [(usize, &str); 7] = [
            (
                count::<ClaimStream<Source>>(
                    Path::new("testdata/camt027_claim_non_receipt.xml"),
                    "read_camt027",
                ),
                "camt.027",
            ),
            (
                count::<AddtlInfStream<Source>>(
                    Path::new("testdata/camt028_additional_info.xml"),
                    "read_camt028",
                ),
                "camt.028",
            ),
            (
                count::<CaseNtfctnStream<Source>>(
                    Path::new("testdata/camt030_case_assignment.xml"),
                    "read_camt030",
                ),
                "camt.030",
            ),
            (
                count::<RjctStream<Source>>(
                    Path::new("testdata/camt031_reject_investigation.xml"),
                    "read_camt031",
                ),
                "camt.031",
            ),
            (
                count::<DbtRspnStream<Source>>(
                    Path::new("testdata/camt036_debit_authorised.xml"),
                    "read_camt036",
                ),
                "camt.036",
            ),
            (
                count::<DbtReqStream<Source>>(
                    Path::new("testdata/camt037_debit_authorisation.xml"),
                    "read_camt037",
                ),
                "camt.037",
            ),
            (
                count::<ModfyStream<Source>>(
                    Path::new("testdata/camt087_modify_payment.xml"),
                    "read_camt087",
                ),
                "camt.087",
            ),
        ];
        for (rows, family) in cases {
            assert_eq!(rows, 1, "{family}: one message, one row");
        }

        // The 2005 first edition puts the versioned identifier where the
        // container name goes, which is the whole reason the latch accepts a
        // `camt.037.` prefix as well as `DbtAuthstnReq`.
        assert_eq!(
            count::<DbtReqStream<Source>>(
                Path::new("testdata/camt037_first_edition.xml"),
                "read_camt037",
            ),
            1,
            "the first edition is a row like any other"
        );
    }

    /// Two messages in one Document. The row is emitted when the container
    /// closes, so the second message has to start from nothing: a carried
    /// assignment would file the second claim under the first one's id, and a
    /// carried `Undrlyg` would give the interbank claim an execution date it
    /// never stated.
    #[test]
    fn a_second_investigation_message_starts_from_nothing() {
        let body = |path: &str| {
            let text = std::fs::read_to_string(path).expect("the fixture is readable");
            let start = text.find("<ClmNonRct>").expect("the container opens");
            let end = text.find("</ClmNonRct>").expect("the container closes");
            text[start..end + "</ClmNonRct>".len()].to_string()
        };
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.027.001.04\">\n\
             {}\n{}\n</Document>\n",
            body("testdata/camt027_claim_non_receipt.xml"),
            body("testdata/camt027_interbank_claim.xml"),
        );
        let path = written("camt027-two-messages.xml", doc.as_bytes());
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<ClaimStream<Source>>::new();
        let rows = pull_batch::<ClaimStream<Source>>(&files, &mut state, "read_camt027")
            .expect("both messages parse");

        assert_eq!(rows.len(), 2, "two containers, two rows");
        assert_eq!(
            rows[0].assignment_id.as_deref(),
            Some("CNRVVVVGB2L200506020001")
        );
        assert_eq!(
            rows[1].assignment_id.as_deref(),
            Some("CNRVVVVGB2L201203020001")
        );
        // the first states an execution date and no settlement date; the second
        // is the interbank arm and states the opposite. Neither may leak.
        assert_eq!(rows[0].original_settlement_date, None);
        assert_eq!(rows[1].original_execution_date, None);
        std::fs::remove_file(&path).ok();
    }

    /// A document that ends between elements, where quick-xml reports `Eof` and
    /// nothing else. Zero rows and no error was the bug: a statement cut off by
    /// a failed transfer came back as a quiet empty table.
    #[test]
    fn input_that_ends_inside_an_element_is_an_error() {
        let doc = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                   <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.03\">\n\
                   <CstmrCdtTrfInitn>\n\
                   <GrpHdr><MsgId>CUT</MsgId></GrpHdr>\n";
        let path = written("pain001-cut-short.xml", doc.as_bytes());
        let files = vec![path.to_string_lossy().into_owned()];
        let mut state = ScanState::<PainStream<Source>>::new();
        let err = pull_batch::<PainStream<Source>>(&files, &mut state, "read_pain001")
            .expect_err("a document that stops inside <CstmrCdtTrfInitn> is an error");
        assert!(
            err.to_string()
                .contains("end of input inside <CstmrCdtTrfInitn>"),
            "the message names the element still open, got {err}"
        );
        std::fs::remove_file(&path).ok();
    }

    /// A FIFO cannot seek, which is the whole reason the two peeked bytes are
    /// handed back to the reader instead of being seeked over. It resolves like
    /// any other local path, so this holds end to end and not just at
    /// `open_source`: compressed or not, a statement may be piped in.
    #[test]
    #[cfg(unix)]
    fn a_statement_may_arrive_down_a_pipe() {
        let want = rows(Path::new(SAMPLE));
        let plain = std::fs::read(SAMPLE).expect("the sample is readable");
        for (name, bytes) in [
            ("pipe.xml", plain.clone()),
            ("pipe.xml.gz", gzipped(&[&plain])),
        ] {
            // Not `written`: that writes the file, and writing to a FIFO blocks
            // until someone reads it. A node left behind by an earlier failure
            // would hang here forever instead of being replaced.
            let path = std::env::temp_dir().join(format!("quackiso-{}-{name}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let made = std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .expect("mkfifo runs");
            assert!(made.success(), "mkfifo {}", path.display());

            // The writer blocks until the scan opens the pipe, so it is spawned
            // first and joined after.
            let feed = {
                let path = path.clone();
                std::thread::spawn(move || std::fs::write(&path, bytes).expect("feed the pipe"))
            };
            let files = resolve_files(&path.to_string_lossy(), "read_iso20022")
                .expect("a fifo is a local path");
            let got = rows_of(&files);
            feed.join().expect("the writer finished");

            assert_eq!(got, want, "{name}");
            std::fs::remove_file(&path).ok();
        }
    }

    /// `glob` rebuilds every match with the platform separator, even a match
    /// with no metacharacter in it, so a path typed with `/` came back with
    /// `\` on Windows and `source_file` stopped naming what the query asked
    /// for.
    #[test]
    fn a_resolved_path_is_spelled_the_way_it_was_asked_for() {
        let files = resolve_files(SAMPLE, "read_iso20022").expect("the sample resolves");
        assert_eq!(files, vec![SAMPLE.to_string()]);
    }
}
