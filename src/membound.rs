//! The memory boundary, measured.
//!
//! `README.md` says a 1.7 GB statement reads in about 2 MB of resident memory.
//! That was a number someone once watched go by: the design notes admitted as
//! much — "nothing yet that measures the memory boundary". This module is the
//! measurement, run as a test, with the terms named.
//!
//! Three numbers, because "2 MB resident" on its own does not say which:
//!
//! * **peak live heap** — the high-water mark of the sum of live allocation
//!   sizes, taken by a tracking global allocator that exists only in test
//!   builds. Deterministic, and the number the bound is actually about.
//! * **peak RSS delta** — `VmHWM` after the parse, with the high-water mark
//!   reset through `/proc/self/clear_refs` immediately before it, minus the
//!   resident set at that moment. Physical pages this parse added. Linux only;
//!   nothing else exposes a resettable peak.
//! * **process peak RSS** — the whole test process, for scale.
//!
//! All three are a **standalone parser measurement**: this is the scan loop
//! `read_iso20022` runs — [`crate::pull_batch`] over an [`EntryStream`], one
//! vector of rows alive at a time — in a test binary with no DuckDB in the
//! process. None of it is an incremental-over-DuckDB figure, and DuckDB's own
//! resident set (tens of MB before a single row is read) is not in it.
//!
//! The bound is not independence from the input. It is
//! `O(VECTOR_SIZE × row + largest subtree)`: one output batch of at most 2048
//! flattened rows, plus the one `<Ntry>` subtree being copied and
//! deserialized. Both terms are exercised here — [`peak_follows_the_output_batch`]
//! widens the row, [`peak_follows_the_largest_subtree`] widens the subtree, and
//! both move the peak. What no input characteristic moves is file size:
//! [`peak_does_not_follow_file_size`] multiplies the statement by eight and the
//! peak does not budge, and [`the_documented_statement`] does it at 1.7 GB.
//! Compression is not an input characteristic either:
//! [`peak_does_not_follow_compression`] hands the same statement over gzipped,
//! where the file shrinks several times over and the decoder works out of a
//! fixed window, so the peak stays the batch it always was.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::mt940::{Mt940Row, Mt940Stream};
use crate::stream::EntryStream;
use crate::{pull_batch, spawn_workers, RowStream, ScanState, Source, VECTOR_SIZE};

// ── what the parse is allowed to cost ────────────────────────────────────────

// The numbers below are what each case measured, not what it is allowed to
// spend. A ceiling four times over the measurement hides a doubling; a band
// does not. An extra copy — here, or in quick-xml, or in serde — moves a peak
// by tens of percent and fails its case with both numbers in the message.
//
// When one fails the fix is to find the copy. If the copy turns out to be
// justified, record the new measurement here and quote it in the commit. Never
// widen a band until the suite goes green.
//
// The heap figures are byte-identical across machines and profiles — the same
// allocations happen in the same order — so the bands are tight. The parallel
// case is the exception: eight workers interleave, and the peak depends on who
// was mid-batch when the consumer drained.

/// One 2048-row batch of ordinary flattened rows, from 4,000 entries to three
/// million. The steady state the streaming claim is about.
const STEADY_HEAP: usize = 1_260 << 10;

/// The same batch when every row carries 4 KiB of remittance text: one batch
/// of wide rows, which is where `VECTOR_SIZE × row` becomes visible.
const WIDE_ROW_HEAP: usize = 9_410 << 10;

/// One 4 MiB `<Ntry>`, then one of 16 MiB: the subtree term, about six times
/// the subtree in both cases.
const SUBTREE_4MIB_HEAP: usize = 25_400 << 10;
const SUBTREE_16MIB_HEAP: usize = 98_970 << 10;

/// The parallel scan is the one case with no recorded value, because it is the
/// one case whose peak is machine-dependent: faster workers keep more batches
/// full at once, and the same code measures 9.3 MiB on a four-core runner and
/// 19.9 MiB on a 32-core box in release. What does not vary is the structure —
/// a batch in flight per worker, twice that queued in the bounded channel, one
/// in the consumer's hand — so this case is held to that formula, and to the
/// glob's length staying out of it.
const PARALLEL_BATCHES: usize = 8 * 3 + 1;

/// Twenty thousand entries copied verbatim out of the corpus. Slightly under
/// the generated steady state: real entries carry fewer fields.
const CORPUS_HEAP: usize = 1_180 << 10;

/// One bare MT940 statement of 2,000 entries, and one of 8,000. The MT bound is
/// the message text plus one output batch: the entries are parsed out of byte
/// ranges as the batch asks for them, so what grows with the statement is the
/// text the framer already holds and not a row per entry.
const MT_STATEMENT_HEAP: usize = 2_541 << 10;
const MT_WIDE_STATEMENT_HEAP: usize = 3_001 << 10;

/// What the gzip decoder itself costs, measured as the difference between the
/// same statement read plain and read gzipped. Fixed state, not per entry:
/// flate2 buffers its input in `vec![0; 32 * 1024]`, miniz_oxide boxes a
/// `TINFL_LZ_DICT_SIZE` window of another 32 KiB, and the huffman tables are
/// about 10 KiB on top. The last three bytes are the two fixtures' names, which
/// the source keeps to put on its own errors. Byte-identical in debug, in
/// release and under musl.
const GZIP_HEAP: usize = 82_217;

/// How far a peak may sit from its recorded value before the case fails.
const BAND: usize = 25;

/// Resident-set ceiling, for the runs where the OS will say. A ceiling rather
/// than a band because RSS carries the allocator's arenas and page granularity
/// on top of the live bytes, and moves between machines; the 1.7 GB statement
/// measures 2.04 MiB of it.
const STEADY_RSS_CEILING: usize = 8 << 20;

/// How much two peaks that should be equal are allowed to differ. Two runs of
/// the same loop over different-sized files allocate the same objects; this
/// covers allocator bookkeeping and the odd byte from a test running beside it.
const NOISE: usize = 256 << 10;

/// Hold a measured peak to what it measured when it was written.
fn holds_at(what: &str, measured: usize, recorded: usize, band: usize) {
    let low = recorded / 100 * (100 - band);
    let high = recorded / 100 * (100 + band);
    assert!(
        (low..=high).contains(&measured),
        "{what}: {} against the {} recorded here, outside ±{band}%. Find the copy \
         that moved; if it belongs there, record it as {} << 10",
        mib(measured),
        mib(recorded),
        (measured + 512) / 1024
    );
}

// ── the allocator the measurement reads ──────────────────────────────────────

/// The system allocator, plus a running total of live bytes and its high-water
/// mark. `mod membound` is `#[cfg(test)]`, so the shipped extension links no
/// allocator shim at all.
struct Tracking;

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn grew(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    /// Forwarded to the system allocator rather than left to the trait's
    /// alloc-copy-dealloc default: a `Vec` that grows in place must be counted
    /// as a bigger allocation, not as a second live one.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = System.realloc(ptr, layout, new_size);
        if !moved.is_null() {
            match new_size.checked_sub(layout.size()) {
                Some(more) => grew(more),
                None => {
                    LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
                }
            }
        }
        moved
    }
}

// ── what the kernel says ─────────────────────────────────────────────────────

/// A `/proc/self/status` size field, in bytes.
#[cfg(target_os = "linux")]
fn status_field(name: &str) -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(name))?;
    let value = line.split_whitespace().nth(1)?;
    Some(value.parse::<usize>().ok()? * 1024)
}

#[cfg(target_os = "linux")]
fn resident() -> Option<usize> {
    status_field("VmRSS:")
}

#[cfg(target_os = "linux")]
fn peak_resident() -> Option<usize> {
    status_field("VmHWM:")
}

/// Drop `VmHWM` back to the current `VmRSS` and report that baseline, so the
/// peak that follows belongs to one parse instead of to the whole process.
/// `5` is `CLEAR_REFS_MM_HIWATER_RSS`. A kernel that refuses gives `None`, and
/// the run reports heap only rather than a peak measured since process start.
#[cfg(target_os = "linux")]
fn reset_peak_resident() -> Option<usize> {
    std::fs::write("/proc/self/clear_refs", b"5\n").ok()?;
    resident()
}

#[cfg(not(target_os = "linux"))]
fn peak_resident() -> Option<usize> {
    None
}

#[cfg(not(target_os = "linux"))]
fn reset_peak_resident() -> Option<usize> {
    None
}

// ── one measured window ──────────────────────────────────────────────────────

struct Sample {
    /// Peak live heap the window added, in bytes.
    heap: usize,
    /// Peak resident set the window added, where the OS will say.
    rss: Option<usize>,
    /// Peak resident set of the whole test process, for scale.
    process: Option<usize>,
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peak live heap {}", mib(self.heap))?;
        match (self.rss, self.process) {
            (Some(rss), Some(process)) => {
                write!(
                    f,
                    ", peak RSS +{} (process peak {})",
                    mib(rss),
                    mib(process)
                )
            }
            // Not a silent skip: the heap bound still holds, but the resident
            // half of the claim is only measurable where the peak can be reset.
            _ => write!(
                f,
                ", resident set not measured (needs Linux /proc/self/clear_refs)"
            ),
        }
    }
}

/// The counters are process-wide and `cargo test` runs tests on parallel
/// threads, so a measurement that did not hold this would measure its
/// neighbours. Held across fixture generation too: writing a 24 MB file
/// allocates. It only excludes the other cases in here, though: a test
/// elsewhere in the suite allocating on another thread still lands in the
/// window, so a run that is not filtered to `membound` needs
/// `--test-threads=1`. Both workflow invocations pass it.
static MEASURING: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `body` and report what it cost. The guard is passed rather than taken
/// here because the fixtures have to be written under it as well.
fn measure<T>(_exclusive: &MutexGuard<'static, ()>, body: impl FnOnce() -> T) -> (T, Sample) {
    let heap_base = LIVE.load(Ordering::Relaxed);
    PEAK.store(heap_base, Ordering::Relaxed);
    let rss_base = reset_peak_resident();

    let out = body();

    let heap = PEAK.load(Ordering::Relaxed).saturating_sub(heap_base);
    let process = peak_resident();
    let rss = match (process, rss_base) {
        (Some(peak), Some(base)) => Some(peak.saturating_sub(base)),
        _ => None,
    };
    (out, Sample { heap, rss, process })
}

// ── the scan under measurement ───────────────────────────────────────────────

/// What a scan produced. The row count alone would let a reader that lost every
/// amount pass a memory test, so the money is added up as the batches go by —
/// exactly as `SUM(amount)` would, in the same scaled integers.
struct Scanned {
    rows: usize,
    total: i128,
}

/// The sequential scan any reader runs: `pull_batch` until it comes back empty,
/// one vector of rows alive at a time. DuckDB copies each batch into its output
/// chunk and drops it; here it is added up and dropped. `amount` is the row's
/// money column, which differs by reader and is the one thing that does.
fn scan_as<S: RowStream>(
    files: &[String],
    fname: &'static str,
    amount: impl Fn(&S::Row) -> Option<i128>,
) -> Scanned {
    let mut state = ScanState::<S>::new();
    let mut out = Scanned { rows: 0, total: 0 };
    loop {
        let batch = pull_batch::<S>(files, &mut state, fname).expect("membound fixtures parse");
        if batch.is_empty() {
            return out;
        }
        out.rows += batch.len();
        out.total += batch.iter().filter_map(&amount).sum::<i128>();
    }
}

/// The camt.053 scan, which is what most of the cases here measure.
fn scan(files: &[String]) -> Scanned {
    scan_as::<EntryStream<Source>>(files, "read_iso20022", |row| row.amount)
}

/// The MT940 scan, whose bound is the message text rather than a row per entry.
fn scan_mt940(files: &[String]) -> Scanned {
    scan_as::<Mt940Stream<Source>>(files, "read_mt940", |row: &Mt940Row| row.amount)
}

/// The parallel scan: workers claim files from the shared counter and hand
/// batches over the bounded channel, and the consumer drains them.
fn scan_parallel(files: Vec<String>, threads: usize) -> Scanned {
    let rx = spawn_workers::<EntryStream<Source>>(files, threads, "read_iso20022");
    let mut out = Scanned { rows: 0, total: 0 };
    for batch in rx {
        let batch = batch.expect("membound fixtures parse");
        out.rows += batch.len();
        out.total += batch.iter().filter_map(|row| row.amount).sum::<i128>();
    }
    out
}

/// What `SUM(amount)` must come to for a generated statement of `entries`
/// entries, in the scaled integers the reader produces: the amount of entry `i`
/// is `(i % 900000) + 100` and `i % 100` cents, and nothing about streaming is
/// allowed to lose one of them.
fn expected_total(entries: usize) -> i128 {
    (0..entries)
        .map(|i| ((i % 900_000) + 100) as i128 * 100_000 + (i % 100) as i128 * 1_000)
        .sum()
}

/// Every entry came back, and every amount with it. A memory bound measured on
/// a scan that quietly produced empty rows would be a bound on nothing.
fn parsed(fixture: &Fixture, scanned: &Scanned) {
    assert_eq!(
        scanned.rows, fixture.entries,
        "the fixture must actually parse"
    );
    assert_eq!(
        scanned.total,
        expected_total(fixture.entries),
        "the rows arrived but the money did not"
    );
}

/// The same for an MT940 fixture, whose money has its own closed form.
fn parsed_mt940(fixture: &Fixture, scanned: &Scanned, statements: usize, entries: usize) {
    assert_eq!(
        scanned.rows, fixture.entries,
        "the fixture must actually parse"
    );
    assert_eq!(
        scanned.total,
        expected_mt940_total(statements, entries),
        "the rows arrived but the money did not"
    );
}

// ── fixtures ─────────────────────────────────────────────────────────────────

/// A generated statement on disk — up to 1.7 GB of it — removed when the test
/// drops it, unless the caller asked to keep it.
struct Fixture {
    path: PathBuf,
    entries: usize,
    bytes: u64,
    keep: bool,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Fixture {
    fn arg(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Set `QUACKISO_MEMBOUND_KEEP=<dir>` to write the fixtures there, under a
/// stable name, and leave them behind. That is how the same bytes reach
/// `scripts/measure_in_duckdb.py`: one generator, two measurements, no second
/// definition of what "the documented statement" is.
fn fixture_path(tag: &str, ext: &str) -> (PathBuf, bool) {
    match std::env::var_os("QUACKISO_MEMBOUND_KEEP") {
        Some(dir) => (
            PathBuf::from(dir).join(format!("quackiso-membound-{tag}.{ext}")),
            true,
        ),
        None => (
            std::env::temp_dir().join(format!(
                "quackiso-membound-{tag}-{}.{ext}",
                std::process::id()
            )),
            false,
        ),
    }
}

const HEAD: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.053.001.02\">\n",
    "  <BkToCstmrStmt>\n",
    "    <GrpHdr><MsgId>MEMBOUND-MSG</MsgId><CreDtTm>2026-07-01T00:00:00</CreDtTm></GrpHdr>\n",
    "    <Stmt>\n",
    "      <Id>MEMBOUND-STMT</Id>\n",
    "      <Acct><Id><IBAN>DE89370400440532013000</IBAN></Id></Acct>\n",
);

const TAIL: &str = concat!("    </Stmt>\n", "  </BkToCstmrStmt>\n", "</Document>\n");

/// The remittance text of an ordinary entry: 28 bytes, which puts the whole
/// entry at 576 and three million of them at 1.7 GB — the shape behind the
/// documented pair.
fn ordinary(i: usize) -> String {
    format!("Invoice {i:012} settled")
}

/// The same text stretched to `width` bytes, for the runs that widen a row or a
/// subtree on purpose.
fn padded(i: usize, width: usize) -> String {
    let base = format!("{} ", ordinary(i));
    let mut text = base.repeat(width / base.len() + 1);
    text.truncate(width);
    text
}

/// Wrap whatever `body` writes in a camt.053 document and record what it cost
/// on disk.
fn fixture(tag: &str, entries: usize, body: impl FnOnce(&mut BufWriter<File>)) -> Fixture {
    let (path, keep) = fixture_path(tag, "xml");
    let file = File::create(&path).expect("membound fixture is writable");
    let mut out = BufWriter::with_capacity(1 << 20, file);
    out.write_all(HEAD.as_bytes()).expect("fixture head");
    body(&mut out);
    out.write_all(TAIL.as_bytes()).expect("fixture tail");
    out.flush().expect("fixture flush");
    drop(out);

    let bytes = std::fs::metadata(&path).expect("fixture exists").len();
    Fixture {
        path,
        entries,
        bytes,
        keep,
    }
}

/// Write a camt.053 statement of `entries` booked entries. `remittance` decides
/// how much unstructured text each one carries, which is how the subtree and
/// batch terms of the bound get moved independently of the file size.
fn statement(tag: &str, entries: usize, remittance: &dyn Fn(usize) -> String) -> Fixture {
    fixture(tag, entries, |out| {
        for i in 0..entries {
            let whole = (i % 900_000) + 100;
            let cents = i % 100;
            let remit = remittance(i);
            write!(
                out,
                r#"    <Ntry>
      <NtryRef>NTRY-{i:012}</NtryRef>
      <Amt Ccy="EUR">{whole}.{cents:02}</Amt>
      <CdtDbtInd>DBIT</CdtDbtInd>
      <Sts><Cd>BOOK</Cd></Sts>
      <BookgDt><Dt>2026-07-01</Dt></BookgDt>
      <ValDt><Dt>2026-07-02</Dt></ValDt>
      <NtryDtls><TxDtls>
        <Refs><EndToEndId>E2E-{i:012}</EndToEndId><TxId>TX-{i:012}</TxId></Refs>
        <RltdPties><Dbtr><Nm>Debtor {i:09}</Nm></Dbtr><Cdtr><Nm>Creditor {i:09}</Nm></Cdtr></RltdPties>
        <RmtInf><Ustrd>{remit}</Ustrd></RmtInf>
      </TxDtls></NtryDtls>
    </Ntry>
"#
            )
            .expect("fixture entry");
        }
    })
}

fn pacs008_credit_transfer(tag: &str, remittance_width: usize) -> Fixture {
    let remit = padded(0, remittance_width);
    fixture(tag, 1, |out| {
        write!(
            out,
            r#"    <FIToFICstmrCdtTrf>
      <GrpHdr><MsgId>MEMBOUND-PACS008</MsgId><IntrBkSttlmDt>2026-07-01</IntrBkSttlmDt></GrpHdr>
      <CdtTrfTxInf>
        <PmtId><EndToEndId>E2E-CAP</EndToEndId><TxId>TX-CAP</TxId></PmtId>
        <IntrBkSttlmAmt Ccy="EUR">100.00</IntrBkSttlmAmt>
        <RmtInf><Ustrd>{remit}</Ustrd></RmtInf>
      </CdtTrfTxInf>
    </FIToFICstmrCdtTrf>
"#
        )
        .expect("fixture credit transfer");
    })
}

/// Write `statements` bare MT940 bodies of `entries` entries each: `:20:`
/// onwards, no blocks at all, which is how a bank ships a statement file. Each
/// body ends at its own `-`, so the framer has a boundary to find.
///
/// The amount of entry `i` is `i` hundred, which gives the money a closed form
/// the same way the camt generator does.
fn mt940_statement(tag: &str, statements: usize, entries: usize) -> Fixture {
    let (path, keep) = fixture_path(tag, "txt");
    let file = File::create(&path).expect("membound fixture is writable");
    let mut out = BufWriter::with_capacity(1 << 20, file);
    for s in 0..statements {
        write!(
            out,
            ":20:MEMBOUND-{s:06}\n\
             :25:GB29NWBK60161331926819\n\
             :28C:{:05}/00001\n\
             :60F:C260819EUR1000,00\n",
            s + 1
        )
        .expect("fixture statement head");
        for i in 0..entries {
            write!(
                out,
                ":61:2608190819C{i}00,00NTRFREF-{i:09}\n:86:NARRATIVE {i:09}\n"
            )
            .expect("fixture entry");
        }
        out.write_all(
            b":62F:C260819EUR1000,00\n\
              :64:C260819EUR1000,00\n\
              -\n",
        )
        .expect("fixture statement tail");
    }
    out.flush().expect("fixture flush");
    drop(out);

    let bytes = std::fs::metadata(&path).expect("fixture exists").len();
    Fixture {
        path,
        entries: statements * entries,
        bytes,
        keep,
    }
}

/// What `SUM(amount)` must come to for [`mt940_statement`]: entry `i` of every
/// statement carries `i` hundred, in the scaled integers the reader produces.
fn expected_mt940_total(statements: usize, entries: usize) -> i128 {
    statements as i128
        * (0..entries)
            .map(|i| i as i128 * 100 * 100_000)
            .sum::<i128>()
}

/// The same statement, gzipped beside itself. Compression belongs to the bytes
/// on disk, so the fixture that comes back carries the same entries and its own
/// smaller size.
fn gzipped(plain: &Fixture) -> Fixture {
    // Appended, not substituted: the plain fixture may be `.txt`, and
    // `with_extension` would drop it.
    let mut path = plain.path.clone().into_os_string();
    path.push(".gz");
    let path = PathBuf::from(path);
    let out = File::create(&path).expect("membound fixture is writable");
    let mut enc = GzEncoder::new(
        BufWriter::with_capacity(1 << 20, out),
        Compression::default(),
    );
    let mut src = BufReader::with_capacity(
        1 << 20,
        File::open(&plain.path).expect("the plain fixture exists"),
    );
    std::io::copy(&mut src, &mut enc).expect("gzip the fixture");
    enc.finish()
        .expect("finish the gzip stream")
        .flush()
        .expect("fixture flush");

    let bytes = std::fs::metadata(&path).expect("fixture exists").len();
    Fixture {
        path,
        entries: plain.entries,
        bytes,
        keep: plain.keep,
    }
}

/// Corpus files whose entries are copied verbatim into the real-shape fixture.
/// All four carry a default namespace, so their `<Ntry>` subtrees are
/// well-formed inside any camt document; between them they cover `<Sts>BOOK</Sts>`
/// and `<Sts><Cd>`, five-fraction-digit amounts, the camt.054 notification
/// shape, and the camt.053.001.08 `Pty/Nm` nesting.
const CORPUS_SHAPES: [&str; 4] = [
    "camt053_sample.xml",
    "camt053_decimal_sample.xml",
    "camt053_v8_sample.xml",
    "camt054_sample.xml",
];

/// Everything outside an XML comment. The corpus files explain themselves in
/// comments, and those comments quote the tags they are about — `<Ntry>` among
/// them — so a harvester that reads them takes a "subtree" that starts inside
/// prose and ends in another element.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find("-->") {
            Some(close) => rest = &after[close + "-->".len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every `<Ntry>` those files hold, verbatim — indentation, element order and
/// all. Generated entries are uniform by construction, which is exactly what a
/// memory bound should not be measured on alone.
fn real_entries() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut entries = Vec::new();
    for name in CORPUS_SHAPES {
        let path = dir.join(name);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let text = without_comments(&text);
        let mut rest = text.as_str();
        while let Some(start) = rest.find("<Ntry>") {
            let from = &rest[start..];
            let end = from
                .find("</Ntry>")
                .unwrap_or_else(|| panic!("{name}: an entry never closes"))
                + "</Ntry>".len();
            entries.push(format!("    {}\n", &from[..end]));
            rest = &from[end..];
        }
    }
    assert!(
        entries.len() >= 10,
        "the corpus shapes yielded only {} entries; a fixture changed shape",
        entries.len()
    );
    entries
}

/// A statement of `entries` entries, cycled through the real shapes.
fn corpus_statement(tag: &str, entries: usize) -> Fixture {
    let shapes = real_entries();
    fixture(tag, entries, |out| {
        for i in 0..entries {
            out.write_all(shapes[i % shapes.len()].as_bytes())
                .expect("fixture entry");
        }
    })
}

// ── reporting ────────────────────────────────────────────────────────────────

fn mib(bytes: usize) -> String {
    format!("{:.2} MiB", bytes as f64 / (1 << 20) as f64)
}

fn on_disk(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

/// One line per measurement, so `--nocapture` prints a table rather than a
/// verdict. The claim in the README is only worth what these lines say.
fn report(label: &str, fixture: &Fixture, rows: usize, sample: &Sample) {
    println!(
        "[membound] {label}: {rows} rows, {} entries, {} on disk -> {sample}",
        fixture.entries,
        on_disk(fixture.bytes),
    );
    if fixture.keep {
        println!("[membound] kept at {}", fixture.path.display());
    }
}

// ── the tests ────────────────────────────────────────────────────────────────

/// Eight times the file, the same peak. This is the property the README claims
/// and the one the parallel and batch terms are measured against.
#[test]
fn peak_does_not_follow_file_size() {
    let lock = exclusive();
    let small = statement("small", 4_000, &ordinary);
    let large = statement("large", 32_000, &ordinary);

    let (small_scan, small_peak) = measure(&lock, || scan(&[small.arg()]));
    let (large_scan, large_peak) = measure(&lock, || scan(&[large.arg()]));
    report("4k entries", &small, small_scan.rows, &small_peak);
    report("32k entries", &large, large_scan.rows, &large_peak);

    parsed(&small, &small_scan);
    parsed(&large, &large_scan);
    assert!(
        large.bytes > small.bytes * 7,
        "the large fixture is meant to be ~8x the small one: {} vs {} bytes",
        large.bytes,
        small.bytes
    );

    assert!(
        large_peak.heap <= small_peak.heap + NOISE,
        "peak follows file size: {} for {} bytes against {} for {} bytes",
        mib(large_peak.heap),
        large.bytes,
        mib(small_peak.heap),
        small.bytes
    );
    holds_at("steady state", large_peak.heap, STEADY_HEAP, BAND);
    if let Some(rss) = large_peak.rss {
        assert!(
            rss <= STEADY_RSS_CEILING,
            "steady-state RSS {} is over the {} ceiling",
            mib(rss),
            mib(STEADY_RSS_CEILING)
        );
    }
}

/// The same rows out of a file several times smaller. What a decoder costs is
/// its own fixed state -- an input buffer, an LZ77 window, huffman tables -- and
/// nothing per entry, so the peak is the batch it always was.
#[test]
fn peak_does_not_follow_compression() {
    let lock = exclusive();
    let plain = statement("gzip", 32_000, &ordinary);
    let zipped = gzipped(&plain);

    let (plain_scan, plain_peak) = measure(&lock, || scan(&[plain.arg()]));
    let (zipped_scan, zipped_peak) = measure(&lock, || scan(&[zipped.arg()]));
    report("32k entries", &plain, plain_scan.rows, &plain_peak);
    report(
        "32k entries gzipped",
        &zipped,
        zipped_scan.rows,
        &zipped_peak,
    );

    parsed(&plain, &plain_scan);
    parsed(&zipped, &zipped_scan);
    assert!(
        zipped.bytes * 4 < plain.bytes,
        "the gzipped fixture is meant to be several times smaller: {} vs {} bytes",
        zipped.bytes,
        plain.bytes
    );

    let decoder = zipped_peak.heap.saturating_sub(plain_peak.heap);
    println!("[membound] the decoder adds {decoder} bytes");
    holds_at("the gzip decoder", decoder, GZIP_HEAP, BAND);
    holds_at(
        "a gzipped statement",
        zipped_peak.heap,
        STEADY_HEAP + GZIP_HEAP,
        BAND,
    );
}

/// The first term of the bound: 2048 rows are alive at once, so a row carrying
/// 4 KiB of remittance text costs 2048 × 4 KiB more than a narrow one. Memory
/// is bounded, not independent of what the rows contain.
#[test]
fn peak_follows_the_output_batch() {
    const WIDE: usize = 4 << 10;
    let lock = exclusive();
    let narrow = statement("narrow", 6_000, &ordinary);
    let wide = statement("wide", 6_000, &|i| padded(i, WIDE));

    let (narrow_scan, narrow_peak) = measure(&lock, || scan(&[narrow.arg()]));
    let (wide_scan, wide_peak) = measure(&lock, || scan(&[wide.arg()]));
    report("narrow rows", &narrow, narrow_scan.rows, &narrow_peak);
    report("4 KiB rows", &wide, wide_scan.rows, &wide_peak);

    parsed(&narrow, &narrow_scan);
    parsed(&wide, &wide_scan);
    holds_at(
        "one batch of narrow rows",
        narrow_peak.heap,
        STEADY_HEAP,
        BAND,
    );
    holds_at(
        "one batch of 4 KiB rows",
        wide_peak.heap,
        WIDE_ROW_HEAP,
        BAND,
    );

    let batch = VECTOR_SIZE * WIDE;
    let grew = wide_peak.heap.saturating_sub(narrow_peak.heap);
    assert!(
        grew >= batch / 2,
        "a batch of 4 KiB rows should cost about {}, it cost {}",
        mib(batch),
        mib(grew)
    );
    assert!(
        grew <= batch * 3,
        "a batch of 4 KiB rows cost {}, more than three times the {} the batch holds",
        mib(grew),
        mib(batch)
    );
}

/// The second term, and the one nothing in the docs admitted to: the largest
/// single subtree. An entry is copied out of the event stream, deserialized,
/// and flattened into a row, so a fat `<Ntry>` is live several times over while
/// that happens. The peak tracks it — quadruple the subtree, quadruple the
/// peak — which is why "independent of the input" is the wrong claim and
/// `O(batch + largest subtree)` is the right one.
#[test]
fn peak_follows_the_largest_subtree() {
    const SMALL: usize = 4 << 20;
    const LARGE: usize = 16 << 20;
    let lock = exclusive();

    let mut peaks = Vec::new();
    for (tag, huge, recorded) in [
        ("subtree4", SMALL, SUBTREE_4MIB_HEAP),
        ("subtree16", LARGE, SUBTREE_16MIB_HEAP),
    ] {
        let fixture = statement(tag, 200, &|i| {
            if i == 0 {
                padded(i, huge)
            } else {
                ordinary(i)
            }
        });
        let (scanned, peak) = measure(&lock, || scan(&[fixture.arg()]));
        report(
            &format!("one {} entry", mib(huge)),
            &fixture,
            scanned.rows,
            &peak,
        );
        parsed(&fixture, &scanned);
        assert!(
            peak.heap >= huge,
            "the subtree is copied and deserialized, so the peak cannot be under its {}: {}",
            mib(huge),
            mib(peak.heap)
        );
        // Copy buffer (doubling as it grows), the deserialized string, and the
        // row's own copy, all live at once: about six times the subtree.
        holds_at(
            &format!("a {} subtree", mib(huge)),
            peak.heap,
            recorded,
            BAND,
        );
        peaks.push(peak.heap);
    }

    // Four times the subtree, about four times the peak: the dependence is
    // linear in the subtree, not in the file, which is 200 entries either way.
    let (small_peak, large_peak) = (peaks[0], peaks[1]);
    assert!(
        large_peak >= small_peak * 3,
        "quadrupling the subtree moved the peak only from {} to {}",
        mib(small_peak),
        mib(large_peak)
    );
}

/// The term compression does decouple, and the reason "compression is free" is
/// the wrong summary. A subtree used to be bounded from above by the file it came
/// from: a 16 MiB `<Ntry>` needed 16 MiB on disk. Gzipped it needs a hundredth of
/// that, and the peak is still six times the entry -- what bounds this term is
/// the inflated size, which `ls` no longer shows.
#[test]
fn a_small_gzip_can_carry_a_large_subtree() {
    const HUGE: usize = 16 << 20;
    let lock = exclusive();
    let plain = statement("gzsubtree", 200, &|i| {
        if i == 0 {
            padded(i, HUGE)
        } else {
            ordinary(i)
        }
    });
    let zipped = gzipped(&plain);

    let (scanned, peak) = measure(&lock, || scan(&[zipped.arg()]));
    report(
        &format!("one {} entry gzipped", mib(HUGE)),
        &zipped,
        scanned.rows,
        &peak,
    );

    parsed(&zipped, &scanned);
    assert!(
        zipped.bytes < HUGE as u64 / 100,
        "the point of this case is a file that hides the subtree: {} bytes on disk",
        zipped.bytes
    );
    assert!(
        peak.heap >= HUGE,
        "the subtree is inflated, copied and deserialized, so the peak cannot be \
         under its {}: {}",
        mib(HUGE),
        mib(peak.heap)
    );
    // The same recorded value as the uncompressed case: `GZIP_HEAP` is 82,217
    // bytes against a peak of about 97 MiB, which is inside the band either way.
    holds_at(
        "a gzipped 16 MiB subtree",
        peak.heap,
        SUBTREE_16MIB_HEAP,
        BAND,
    );
}

#[test]
#[ignore = "writes a gzipped fixture with a 64 MiB transaction subtree"]
fn a_gzipped_credit_transfer_past_the_subtree_cap_is_rejected() {
    const HUGE: usize = (64 << 20) + 1;
    let plain = pacs008_credit_transfer("pacs008cap", HUGE);
    let zipped = gzipped(&plain);
    let mut state = ScanState::<crate::pacs008::TxStream<Source>>::new();

    let err = match pull_batch::<crate::pacs008::TxStream<Source>>(
        &[zipped.arg()],
        &mut state,
        "read_pacs008",
    ) {
        Ok(batch) => panic!("oversized credit transfer parsed as {} rows", batch.len()),
        Err(err) => err,
    };
    let text = err.to_string();
    assert!(
        text.contains("<CdtTrfTxInf> exceeds the 67108864 byte subtree cap"),
        "{text}"
    );
}

/// The parallel scan multiplies the bound by the worker count and the channel
/// depth, not by the size of the glob: three times the corpus against the same
/// eight workers, held to the batches that can be in flight at once. The ceiling
/// is the assertion and not a ratio between two runs, because the peak of an
/// interleaving moves with the machine and the glob's length is not in it.
#[test]
fn parallel_peak_follows_threads_not_corpus() {
    const THREADS: usize = 8;
    let lock = exclusive();
    let fixtures: Vec<Fixture> = (0..24)
        .map(|n| statement(&format!("par{n}"), 2_500, &ordinary))
        .collect();
    let eight: Vec<String> = fixtures[..THREADS].iter().map(Fixture::arg).collect();
    let all: Vec<String> = fixtures.iter().map(Fixture::arg).collect();
    let corpus: u64 = fixtures.iter().map(|f| f.bytes).sum();

    let (eight_scan, eight_peak) = measure(&lock, || scan_parallel(eight, THREADS));
    let (all_scan, all_peak) = measure(&lock, || scan_parallel(all, THREADS));
    println!(
        "[membound] 8 files, {THREADS} workers: {} rows -> {eight_peak}\n\
         [membound] 24 files ({}), {THREADS} workers: {} rows -> {all_peak}",
        eight_scan.rows,
        on_disk(corpus),
        all_scan.rows
    );

    // Every worker's rows, and every worker's money: a file claimed twice or
    // dropped shows up here before it shows up in a peak.
    assert_eq!(eight_scan.rows, THREADS * 2_500);
    assert_eq!(all_scan.rows, 24 * 2_500);
    assert_eq!(eight_scan.total, expected_total(2_500) * THREADS as i128);
    assert_eq!(all_scan.total, expected_total(2_500) * 24);

    let inflight = PARALLEL_BATCHES * STEADY_HEAP;
    assert!(
        all_peak.heap <= inflight,
        "eight workers peaked at {}, over the {} that {PARALLEL_BATCHES} batches \
         in flight can hold: the channel is no longer bounding anything",
        mib(all_peak.heap),
        mib(inflight)
    );
}

/// Every other fixture here is generated, and generated entries are uniform by
/// construction — one shape, one width, one element order. This one cycles
/// verbatim `<Ntry>` subtrees out of the real corpus, so the bound is measured
/// on the shapes the readers were actually written against.
#[test]
fn peak_is_bounded_on_real_entry_shapes() {
    let lock = exclusive();
    let fixture = corpus_statement("corpus", 20_000);

    let (scanned, peak) = measure(&lock, || scan(&[fixture.arg()]));
    report("real corpus shapes", &fixture, scanned.rows, &peak);

    assert_eq!(
        scanned.rows, fixture.entries,
        "the fixture must actually parse"
    );
    // Real amounts, so there is no closed form to check against — but a scan
    // that lost them would come to nothing at all.
    assert!(
        scanned.total > 0,
        "20,000 real entries summed to {}",
        scanned.total
    );
    holds_at("real entry shapes", peak.heap, CORPUS_HEAP, BAND);
}

/// Eight statements against sixty-four, the same entries in each: the MT framer
/// releases every message it hands over, so the file does not enter the bound.
#[test]
fn mt_peak_does_not_follow_file_size() {
    let lock = exclusive();
    let small = mt940_statement("mt-small", 8, 2_000);
    let large = mt940_statement("mt-large", 64, 2_000);

    let (small_scan, small_peak) = measure(&lock, || scan_mt940(&[small.arg()]));
    let (large_scan, large_peak) = measure(&lock, || scan_mt940(&[large.arg()]));
    report(
        "8 statements x 2k entries",
        &small,
        small_scan.rows,
        &small_peak,
    );
    report(
        "64 statements x 2k entries",
        &large,
        large_scan.rows,
        &large_peak,
    );

    parsed_mt940(&small, &small_scan, 8, 2_000);
    parsed_mt940(&large, &large_scan, 64, 2_000);
    assert!(
        large.bytes > small.bytes * 7,
        "the large fixture is meant to be ~8x the small one: {} vs {} bytes",
        large.bytes,
        small.bytes
    );

    assert!(
        large_peak.heap <= small_peak.heap + NOISE,
        "peak follows file size: {} for {} bytes against {} for {} bytes",
        mib(large_peak.heap),
        large.bytes,
        mib(small_peak.heap),
        small.bytes
    );
}

/// The bound MT has once nothing per entry is retained: the message text plus one
/// output batch. Four times the entries per statement is four times the text, and
/// the peak moves by that and by allocator noise - a `Vec` of regions or of rows
/// would show up here as a term the text does not explain.
#[test]
fn mt_peak_follows_the_message_text() {
    let lock = exclusive();
    let small = mt940_statement("mt-narrow", 1, 2_000);
    let large = mt940_statement("mt-wide", 1, 8_000);

    let (small_scan, small_peak) = measure(&lock, || scan_mt940(&[small.arg()]));
    let (large_scan, large_peak) = measure(&lock, || scan_mt940(&[large.arg()]));
    report(
        "1 statement x 2k entries",
        &small,
        small_scan.rows,
        &small_peak,
    );
    report(
        "1 statement x 8k entries",
        &large,
        large_scan.rows,
        &large_peak,
    );

    parsed_mt940(&small, &small_scan, 1, 2_000);
    parsed_mt940(&large, &large_scan, 1, 8_000);

    let text = (large.bytes - small.bytes) as usize;
    assert!(
        large_peak.heap <= small_peak.heap + text + NOISE,
        "six thousand more entries moved the peak from {} to {}, past the {} of \
         text they added and {} of allocator noise: something still scales with \
         the entry count",
        mib(small_peak.heap),
        mib(large_peak.heap),
        mib(text),
        mib(NOISE)
    );
    holds_at(
        "one MT940 statement",
        small_peak.heap,
        MT_STATEMENT_HEAP,
        BAND,
    );
    holds_at(
        "a four times wider MT940 statement",
        large_peak.heap,
        MT_WIDE_STATEMENT_HEAP,
        BAND,
    );
}

/// The documented statement, reproduced: three million entries, 1.7 GB on disk.
/// Ignored by default — it writes 1.7 GB to the temp directory and takes
/// minutes in a debug build:
///
/// ```text
/// cargo test --release membound -- --ignored --nocapture
/// ```
///
/// `QUACKISO_MEMBOUND_ENTRIES` scales the statement down for a run that has to
/// fit somewhere smaller — CI, where the point is that the measurement runs at
/// all. The 1.7 GB shape check applies only at the documented count, because
/// only that count is what README.md quotes.
#[test]
#[ignore = "writes a 1.7 GB fixture; run it deliberately"]
fn the_documented_statement() {
    const DOCUMENTED: usize = 3_000_000;
    let entries = match std::env::var("QUACKISO_MEMBOUND_ENTRIES") {
        Ok(text) => text
            .trim()
            .parse()
            .expect("QUACKISO_MEMBOUND_ENTRIES is an entry count"),
        Err(_) => DOCUMENTED,
    };

    let lock = exclusive();
    let fixture = statement("documented", entries, &ordinary);
    if entries == DOCUMENTED {
        assert!(
            (1_600_000_000..1_800_000_000).contains(&fixture.bytes),
            "the fixture is meant to be the documented 1.7 GB shape, it is {} bytes",
            fixture.bytes
        );
    }

    let (scanned, peak) = measure(&lock, || scan(&[fixture.arg()]));
    report("the documented statement", &fixture, scanned.rows, &peak);

    parsed(&fixture, &scanned);
    holds_at(
        &format!("{} of statement", on_disk(fixture.bytes)),
        peak.heap,
        STEADY_HEAP,
        BAND,
    );
    if let Some(rss) = peak.rss {
        assert!(
            rss <= STEADY_RSS_CEILING,
            "{} cost {} of resident memory, over the {} ceiling",
            on_disk(fixture.bytes),
            mib(rss),
            mib(STEADY_RSS_CEILING)
        );
    }
}
