# 10. A declared encoding is honoured, an undeclared one is not guessed

Status: accepted

Decided with the 1.9.0 release. The readers were UTF-8 only, and the arguable
part is not the transcoding but where it stops.

## Context

A bank's file is not always UTF-8. A Latin-1 camt.053 from a German savings
bank, a Windows-1251 statement from a Russian-language ledger and a UTF-16 export
from a Windows treasury system all say what they are -- the first two in the XML
declaration (`encoding="ISO-8859-1"`), the third in its byte order mark -- and
all three were unreadable. quick-xml, built without its `encoding` feature,
passes the bytes through and fails on the first one that is not UTF-8, in
whichever text node or subtree copy meets it first:

```text
Malformed UTF-8 input: invalid utf-8 sequence of 1 bytes from index 471
```

and a UTF-16 file failed earlier and less legibly, as a file with no markup in
it. Neither error names a way out. The SWIFT MT readers had the same failure with
a better message: a `:86:` narrative in a legacy code page stopped the scan with
`not UTF-8 text` and an index.

What the sender states and what the reader infers are different things. A
declaration or a byte order mark is a statement by the writer, and reading the
file by it is reading the file. A guess about undeclared bytes is the reader
stating something the file never said.

## Decision

A declared encoding is honoured; an undeclared one is not guessed.

`Input` decides this on the prefix the shape guard already reads, before any
reader sees a byte:

- a UTF-16 byte order mark (`FF FE`, `FE FF`) names the encoding and is skipped,
  and a UTF-8 byte order mark is UTF-8 as it always was;
- otherwise an `<?xml ... encoding="X"?>` in the first 512 bytes, scanned as
  ASCII, names it through `Encoding::for_label`, which knows the WHATWG labels;
- UTF-8, or no declaration, is the path that existed before, untouched;
- a label `encoding_rs` does not know is an error naming the file and the label
  (`declared encoding "x-klingon" is not supported`), because reading it as UTF-8
  anyway is the guess this refuses to make;
- a declaration of UTF-16 in ASCII text, with no byte order mark, is the same
  error with a reason: a UTF-16 file is not ASCII-readable, so a file whose
  declaration can be read as ASCII and says UTF-16 is not what it says it is.

Any other declared encoding is decoded to UTF-8 by `encoding_rs` as the file is
read. The prefix is decoded once, held as the replay, and the decoder carries on
from where it stopped, so a character the 64 KiB prefix ends inside is completed
by the next read. Everything above `Input` -- the guards, the container check,
the shape decision, the sniffer, the MT framer and the XML readers -- sees UTF-8
and nothing about the file's own encoding. Gzip is unwrapped before any of this,
so a gzipped Latin-1 file reads the same.

The XML declaration is not rewritten. quick-xml ignores what the declaration
says, so a decoded stream that still reads `encoding="ISO-8859-1"` parses and
every reader returns the text as UTF-8.

The decode does not substitute. `Decoder::decode_to_utf8` writes U+FFFD for a
byte its encoding cannot read; this uses the variant that reports it instead, and
a malformed sequence -- or one cut short by the end of the file -- is an error
naming the file and the encoding. A replacement character in a counterparty name
is a changed fact in a payment file, which ADR 0001 forbids for an amount and
ADR 0008 forbids as an invention. In practice the single-byte encodings cannot
fail this way, since every byte maps to something, and the error is for UTF-16
and the multi-byte encodings.

Undeclared input is read as UTF-8 and fails as it did, now saying why:

- XML with no declaration: the parser's text, then ` - the file declares no
  encoding, and XML without a declaration is UTF-8`. A file that declares UTF-8
  and holds a bad byte is not told that it declared nothing.
- SWIFT MT: `not UTF-8 text (...) - SWIFT MT declares no encoding, so convert the
  file first, e.g. iconv -f ISO-8859-1 -t UTF-8`. MT has no declaration of its
  own, so a Latin-1 or cp852 MT file is never transcoded. A byte order mark is a
  declaration wherever it is found, and an MT file that has one is decoded like
  any other.

`ISO-8859-1` is decoded as Windows-1252, as `encoding_rs` does for every
WHATWG label: the two agree except on 0x80-0x9F, which Windows-1252 maps to
printable characters (`0x80` is the euro sign) and ISO-8859-1 to control codes
no bank file means.

## Alternatives rejected

**Guessing a charset for undeclared bytes.** A detector finds the encoding that
fits the bytes best, and a wrong guess is silent: Windows-1251 and Windows-1252
decode the same bytes into different words, both without an error, and the name
that comes out is wrong in a way nothing downstream can see. This is the "no
invention" rule of ADR 0008 applied to text, and it is also why the MT readers
are not transcoded: SWIFT MT declares nothing, so there is nothing to honour, and
the error names the conversion the owner of the file knows how to make.

**quick-xml's `encoding` feature.** It decodes events, not the byte stream. The
container check, the MT-before-markup decision, the 64 KiB shape rule and the MT
framer all read bytes before and beside the XML parser, so they would still see a
Latin-1 file as bytes that are not text, and the sniffer's `Reader` would need the
same wiring as every reader. Decoding once, underneath all of them, keeps one
encoding above `Input`.

**Replacing malformed sequences with U+FFFD.** The default of `decode_to_utf8`.
Rejected above: it turns a damaged file into a plausible one.

**Detecting UTF-16 without a byte order mark.** A UTF-16 document is required to
carry one, and `<\0?\0x\0m\0l\0` as a pattern would be a heuristic added for a
writer that breaks the rule.

## Consequences

The memory bound gains one transcoder when a file declares a non-UTF-8 encoding:
an 8 KiB staging buffer for the raw bytes and one of the same size for the decoded
text, plus the decoded prefix, which is held as the replay and is at most three
times its 64 KiB. It is nothing a scan that reads UTF-8 pays, and it is
independent of the file's size.

`Input::read` never reports the end of the file before it has happened. A decoder
may need more raw bytes before it can emit a character, and a caller's buffer may
be shorter than the character it emits, so the transcoder stages its output and
reads again rather than return nothing.

The sniffer does not abort on an encoding it cannot read. Its guard settles the
encoding and keeps any failure for the first read, so the file's row carries it in
`error` as every other content problem does.

The UTF-8 error hint costs a second open of the file, on the error path only,
because the parser that raises the error no longer has the file's head. A file
that cannot be opened again gets no hint rather than a wrong one.

`Encoding` is compared by name. In this build `enc == encoding_rs::UTF_8` was
false for the encoding `Encoding::for_label("utf-8")` returned: the comparison is
by address, and the compiler may duplicate the `static` it points to.

Fixtures: `testdata/camt053_latin1.xml` and `testdata/camt053_utf16.xml` are
`camt053_sample.xml` in the two encodings; `testdata/camt053_undeclared_latin1.xml`
is the same Latin-1 bytes with no declaration; `testdata/camt053_unknown_encoding.xml`
declares an encoding nobody knows; `testdata/mt940_latin1.txt` is an MT940 with one
`0xFC` in a narrative. They are marked binary in `.gitattributes` so a checkout
does not rewrite the bytes that are the test.
