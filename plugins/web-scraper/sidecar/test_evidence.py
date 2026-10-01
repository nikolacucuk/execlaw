import unittest

from evidence import source_evidence


class SourceEvidenceTests(unittest.TestCase):
    def test_source_id_is_stable_across_fragments_and_body_hash_changes(self):
        first = source_evidence(
            "https://example.test/article#section-one",
            "<main>first body</main>",
            "first body",
            128,
        )
        changed = source_evidence(
            "https://example.test/article#section-two",
            "<main>second body</main>",
            "second body",
            128,
        )
        self.assertEqual(first["source_id"], changed["source_id"])
        self.assertNotEqual(first["content_sha256"], changed["content_sha256"])
        self.assertIsInstance(first["retrieved_at"], int)

    def test_retained_snapshot_is_bounded_and_marks_truncation(self):
        evidence = source_evidence(
            "https://example.test/article",
            "<main>long body</main>",
            "useful evidence " * 20,
            64,
        )
        self.assertLessEqual(len(evidence["snapshot_text"]), 64)
        self.assertTrue(evidence["snapshot_truncated"])


if __name__ == "__main__":
    unittest.main()
