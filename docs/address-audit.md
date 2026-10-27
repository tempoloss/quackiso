# audit_addresses

CBPR+ was to stop accepting a fully unstructured postal address on 14 November
2026. On 27 August 2026 Swift delayed the whole November 2026 standards release
for payments, the address removal with it, and promised a revised schedule by
December 2026. Until then the SR2025 rules apply and unstructured, hybrid and
structured addresses are all accepted, so `finding` names what the rule will
refuse once it lands and not what is refused today. The rule itself is
unchanged, and this is still the question that comes before the migration: of
the traffic already on disk, which parties would be refused, and why. MT counts
here as much as MX does, and the same query reads both — an MT `:50K:` is a name
and then free-text lines, which is exactly the shape the rule stops accepting.

```sql
-- Every party that would be refused, and what is wrong with it.
SELECT family, role, town, country, address_format, finding
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
ORDER BY family, role;

-- The migration's scoreboard: how much of the traffic is in which shape.
SELECT address_format, count(*) AS parties, count(finding) AS would_be_refused
FROM audit_addresses('inbox/**/*')
GROUP BY address_format
ORDER BY parties DESC;

-- What the verdict alone does not say: which repair each refusal actually needs.
-- A free-text address whose town is already written needs labelling, a bare one
-- needs the data collected, and a structured address missing `Ctry` needs
-- neither. All three read `finding IS NOT NULL` and are different jobs.
SELECT CASE WHEN address_text IS NULL THEN 'structured, incomplete'
            ELSE 'free text, ' || address_lines || ' line(s) to label' END AS repair,
       count(*) AS parties
FROM audit_addresses('inbox/**/*')
WHERE finding IS NOT NULL
GROUP BY repair
ORDER BY parties DESC;
```

`family`, `message_id`, `record_index`, `party_path`, `role`, `party_kind`,
`name`, `bic`, `town`, `country`, `address_text`, `address_lines`,
`longest_address_line`, `structured_elements`, `address_format`, `finding`,
`source_file`

One row per party occurrence, which is the grain the question has: a party may
be stated once for a whole payment group (`record_index` NULL) or per
transaction, and pacs.008 alone carries five parties and six agents that may
hold an address. `address_format` is read off the wire and is one of
`STRUCTURED` (dedicated elements, no `AdrLine`), `HYBRID` (`AdrLine` beside a
`TwnNm` and a `Ctry` of their own), `UNSTRUCTURED` (`AdrLine` without both of
them) or `NONE`. `finding` is the rule applied to those facts and is NULL when
nothing in the party would be refused, so `count(finding)` is the size of the
job and `finding` itself says which repair it needs — a missing `TwnNm`, a third
address line, or a line past 70 characters. Address lines are measured in
characters, not bytes.

`address_text` is the address lines themselves, newline-joined and in wire order,
NULL when the party carries none. The counts say how far a party is from the
rule; the text says what there is to work with, and the two do not follow from
each other. Real traffic carries `FOOSTREET 65 / MADRID SPAIN 28010` beside a
bare `BEX 99`, and both are refused for having no `TwnNm` and no `Ctry`: the
first needs its town moved into an element, the second needs a town. Deciding
which is which is a reading of the data, so the audit reports the lines and
stops there rather than guessing a town out of free text.

An agent named by a BIC alone needs no address, and a party carrying none may or
may not have needed one: both are `NONE` with no finding, because whether it was
required there is a usage-guideline question this cannot see. The cash-management
and administration families are outside the mandate (camt.052, camt.053,
camt.054, camt.060, camt.025, admi.024) — their parties are reported with their
format and never with a finding, and `family` is on every row so the line is
visible.

On the MT side the shapes are the same distinction spelled in another alphabet. A
`:50K:`, `:50H:` or letterless `:59:` is a name and then free-text lines:
`UNSTRUCTURED`, and the reason the mandate exists. A `:50F:` numbers its
subfields — `1/` name, `2/` address line, `3/` country and town — so `3/BE/BRUSSELS`
fills a `TwnNm` and a `Ctry`, and the party comes out `HYBRID` with no finding.
An institution given as a BIC (`:57A:`) is `NONE`, as it is in XML.

Two MT details are worth knowing. MT numbers its transactions differently in
every message type, so `record_index` is NULL for MT rather than guessed, and
`party_path` locates the party by field tag instead: `50K`, `52A`, and `52A#2`
for the second occurrence — an MT202COV states `:52a:` in both of its sequences.
And the audit reads MT types no reader here covers, MT300 and MT320 among them,
because a party field is a party field whether or not the rest of the message is
understood. Fields 50 to 59 are the party fields of every payment type: 50 and 59
are the customers at each end, everything between them is an institution on the
route. Which end is which depends on the message and not on the tag, so `role` is
read against the message number: field 59 is the beneficiary of an MT103 and the
debtor of an MT104, because a direct debit collects where a credit transfer pays.

A glob fails on the first unreadable file, the way every reader here does, so
auditing a real inbox is two steps: `sniff_iso20022` to find out what is in the
folder, then `audit_addresses` over the files that are messages. ADR 0008
records why this is a function of its own rather than columns on the readers.
