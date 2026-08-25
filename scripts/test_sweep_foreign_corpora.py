#!/usr/bin/env python3
"""The sweep's rules, held without a corpus or a built extension.

`scripts/sweep_foreign_corpora.py` needs a fetched corpus, a generator run and a
loaded extension before it says anything at all, which means the rules
themselves went untested: R8 was added because 18 generated messages went a
whole release unidentified while every rule passed, and a rule that only runs
where the corpus is is a rule nobody can be sure of.

`judge` is a pure function of one verdict and one recorded outcome, so this
feeds it the verdicts the corpus would have produced. Every case here is a shape
the corpus has actually held, or the shape a defect made it hold.

Usage:
    configure/venv/bin/python3 -m unittest scripts.test_sweep_foreign_corpora
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from scripts.sweep_foreign_corpora import (  # noqa: E402
    GENERATED_FIELDS,
    SUPPLEMENTARY,
    generated_outcome,
    judge,
)


def verdict(**overrides: object) -> dict:
    """A verdict with nothing wrong in it, for a case to break one field of."""
    base: dict[str, object] = {
        "file": "camt053__daily_account_statement__01.xml",
        "message_type": "camt.053.001.08",
        "family": "camt.053",
        "reader": "read_iso20022",
        "records": 3,
        "sniff_error": None,
        "sniff_raised": None,
        "rows": 3,
        "reader_error": None,
        "audit_parties": 6,
        "audit_findings": 2,
        "audit_error": None,
    }
    for _, field in SUPPLEMENTARY:
        base[f"{field}_rows"] = 4
        base[f"{field}_error"] = None
    base["balance_rows"] = 2
    base.update(overrides)
    return base


def rules(findings: list[tuple[str, str]]) -> list[str]:
    return [rule for rule, _ in findings]


class GeneratedIdentity(unittest.TestCase):
    """R8: what the release before this one could not see."""

    def test_a_generated_message_the_sniffer_cannot_name_is_a_finding(self):
        found, _ = judge(
            "generated",
            "camt107__cbpr_treasury_cheque__01.xml",
            verdict(
                message_type=None,
                family=None,
                reader=None,
                records=None,
                rows=None,
                sniff_error=(
                    "unrecognised message: <Document> child <ChqPresntmntNtfctn> matches "
                    "no known ISO 20022 message type"
                ),
                **{f"{field}_rows": None for _, field in SUPPLEMENTARY},
            ),
            None,
        )
        self.assertEqual(rules(found), ["R8"])

    def test_a_valid_family_with_no_reader_is_inventory_and_passes(self):
        """The outcome R8 must not be confused with: identified, unsupported."""
        found, _ = judge(
            "generated",
            "camt107__cbpr_treasury_cheque__01.xml",
            verdict(
                message_type="camt.107.001.01",
                family="camt.107",
                reader=None,
                records=None,
                rows=None,
                **{f"{field}_rows": None for _, field in SUPPLEMENTARY},
            ),
            None,
        )
        self.assertEqual(found, [])

    def test_a_truncated_static_file_is_not_an_identity_finding(self):
        """R8 is about the generated tier. The static corpus keeps truncated
        documents on purpose, and `sniff_error` is how the sniffer reports one."""
        found, _ = judge(
            "static",
            "fixtures/invalid/camt053_truncated.xml",
            verdict(sniff_error="not well-formed XML: syntax error: tag not closed"),
            {
                "reader": "read_iso20022",
                "error": None,
                "rows": 3,
                "records": 3,
                "audit_error": None,
                "audit_parties": 6,
                "audit_findings": 2,
            },
        )
        self.assertEqual(found, [])


class SupplementaryReaders(unittest.TestCase):
    """R9: two walks of one statement disagreeing."""

    def test_a_supplementary_reader_raising_where_the_primary_did_not_is_a_finding(self):
        found, _ = judge(
            "generated",
            "camt053__high_volume_batch__01.xml",
            verdict(balance_error='amount "10.1234567" has 7 fraction digits'),
            None,
        )
        self.assertEqual(rules(found), ["R9"])

    def test_each_supplementary_reader_is_named_separately(self):
        found, _ = judge(
            "generated",
            "camt053__high_volume_batch__01.xml",
            verdict(**{f"{field}_error": "syntax error: tag not closed" for _, field in SUPPLEMENTARY}),
            None,
        )
        self.assertEqual(rules(found), ["R9"] * len(SUPPLEMENTARY))
        for name, _ in SUPPLEMENTARY:
            self.assertTrue(
                any(name in detail for _, detail in found),
                f"{name} is not named in {found}",
            )

    def test_a_primary_refusal_does_not_multiply_into_four(self):
        """`read_one` skips the supplementary calls when the primary reader
        raised, so a message of the wrong family is one finding and not five."""
        found, _ = judge(
            "generated",
            "pain001__basic__01.xml",
            verdict(
                reader_error="no <Stmt>, <Ntfctn> or <Rpt> found",
                rows=None,
                **{f"{field}_rows": None for _, field in SUPPLEMENTARY},
            ),
            None,
        )
        self.assertEqual(rules(found), ["R2"])


class EmptyStatements(unittest.TestCase):
    """R10: a statement that states nothing."""

    def test_a_statement_with_neither_entries_nor_balances_is_a_finding(self):
        found, _ = judge(
            "generated",
            "camt053__simplified_statement__01.xml",
            verdict(records=0, rows=0, balance_rows=0),
            None,
        )
        self.assertEqual(rules(found), ["R10"])

    def test_a_statement_of_balances_alone_passes(self):
        found, _ = judge(
            "generated",
            "camt053__year_end_statement__01.xml",
            verdict(records=0, rows=0, balance_rows=4),
            None,
        )
        self.assertEqual(found, [])

    def test_a_notification_with_no_entries_is_not_a_finding(self):
        """camt.054 entries are 0..n and its schema has no <Bal>, so an empty
        notification is a notification and not a contradiction."""
        found, _ = judge(
            "generated",
            "camt054__basic_credit_confirmation__01.xml",
            verdict(family="camt.054", records=0, rows=0, balance_rows=0),
            None,
        )
        self.assertEqual(found, [])

    def test_a_crash_is_reported_rather_than_an_empty_statement(self):
        found, _ = judge(
            "generated",
            "camt052__daily_balance_report__01.xml",
            verdict(
                family="camt.052",
                records=0,
                rows=0,
                balance_rows=None,
                balance_error="syntax error: tag not closed",
            ),
            None,
        )
        self.assertEqual(rules(found), ["R9"])


class RecordingBehaviour(unittest.TestCase):
    """What `--record` may and may not wave through."""

    def test_recording_skips_the_comparison_rules(self):
        """R3 and R6 are the record talking to itself, and the run that
        rewrites the record cannot be held to the one it replaces."""
        found, missing = judge(
            "static",
            "fixtures/valid/camt053.xml",
            verdict(rows=9),
            {
                "reader": "read_iso20022",
                "error": "no <Stmt>, <Ntfctn> or <Rpt> found",
                "rows": 3,
                "records": 3,
                "audit_error": None,
                "audit_parties": 6,
                "audit_findings": 2,
            },
            recording=True,
        )
        self.assertEqual(found, [])
        self.assertEqual(missing, [])

    def test_recording_still_refuses_an_unidentified_generated_message(self):
        found, _ = judge(
            "generated",
            "camt025__fx_rate_update__01.xml",
            verdict(
                message_type=None,
                family=None,
                reader=None,
                records=None,
                rows=None,
                sniff_error="unrecognised message: <Document> child <Rcpt>",
                **{f"{field}_rows": None for _, field in SUPPLEMENTARY},
            ),
            None,
            recording=True,
        )
        self.assertEqual(rules(found), ["R8"])

    def test_recording_still_refuses_a_silent_empty_result(self):
        found, _ = judge(
            "static",
            "fixtures/valid/camt053.xml",
            verdict(rows=0),
            None,
            recording=True,
        )
        self.assertEqual(rules(found), ["R4"])

    def test_an_unrecorded_static_file_is_not_a_finding_while_recording(self):
        found, missing = judge("static", "fixtures/new.xml", verdict(), None, recording=True)
        self.assertEqual(found, [])
        self.assertEqual(missing, [])

    def test_an_unrecorded_static_file_is_reported_on_a_normal_run(self):
        found, missing = judge("static", "fixtures/new.xml", verdict(), None)
        self.assertEqual(found, [])
        self.assertEqual(missing, ["fixtures/new.xml"])


class GeneratedComparison(unittest.TestCase):
    """The recorded generated outcome: what it holds, and what it must not."""

    def test_the_recorded_fields_are_the_deterministic_ones(self):
        got = generated_outcome(verdict())
        self.assertEqual(sorted(got), sorted(GENERATED_FIELDS))
        self.assertNotIn(
            "audit_findings",
            got,
            "how many parties would be refused moves with the invented text",
        )
        self.assertEqual(got["balance_rows"], 2)
        self.assertEqual(got["audit_parties"], 6)

    def test_a_generated_outcome_is_stable_across_two_identical_runs(self):
        self.assertEqual(generated_outcome(verdict()), generated_outcome(verdict()))


class ExistingRules(unittest.TestCase):
    """R1 to R7 still hold, so the three new rules did not displace them."""

    def test_records_above_zero_with_no_rows_is_still_a_silent_empty(self):
        found, _ = judge("generated", "camt053__x__01.xml", verdict(rows=0), None)
        self.assertIn("R4", rules(found))

    def test_an_uncounted_family_with_no_rows_is_still_a_silent_empty(self):
        found, _ = judge(
            "generated",
            "camt027__x__01.xml",
            verdict(
                family="camt.027",
                reader="read_camt027",
                records=None,
                rows=0,
                **{f"{field}_rows": None for _, field in SUPPLEMENTARY},
            ),
            None,
        )
        self.assertEqual(rules(found), ["R5"])

    def test_the_audit_refusing_a_file_a_reader_read_is_still_drift(self):
        found, _ = judge(
            "generated",
            "camt053__x__01.xml",
            verdict(audit_error="no ISO 20022 message found"),
            None,
        )
        self.assertEqual(rules(found), ["R2", "R7"])

    def test_a_sniffer_raise_is_still_a_finding(self):
        found, _ = judge(
            "static",
            "fixtures/valid/camt053.xml",
            verdict(sniff_raised="ZIP archive: extract a member before reading"),
            None,
            recording=True,
        )
        self.assertEqual(rules(found), ["R2"])


if __name__ == "__main__":
    unittest.main()
