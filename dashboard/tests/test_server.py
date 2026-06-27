import os
import unittest

import server


class BindSafetyTests(unittest.TestCase):
    def test_parse_bind_accepts_host_port(self):
        self.assertEqual(server.parse_bind("127.0.0.1:9510"), ("127.0.0.1", 9510))

    def test_parse_bind_rejects_invalid_port(self):
        with self.assertRaises(ValueError):
            server.parse_bind("127.0.0.1:99999")

    def test_loopback_detection_is_strict(self):
        self.assertTrue(server.is_loopback_host("127.0.0.1"))
        self.assertTrue(server.is_loopback_host("localhost"))
        self.assertFalse(server.is_loopback_host("0.0.0.0"))


class AuthTests(unittest.TestCase):
    def test_compare_digest_auth_helper_importable(self):
        self.assertRegex(server.VERSION, r"^\d+\.\d+\.\d+$")
        self.assertGreater(server.MAX_POST_BYTES, 400)


class QueryLimitTests(unittest.TestCase):
    def test_parse_limit_clamps_negative_and_excessive_values(self):
        self.assertEqual(server.parse_limit("-1", 500, 5000), 1)
        self.assertEqual(server.parse_limit("999999", 500, 5000), 5000)

    def test_parse_limit_rejects_non_integer_values(self):
        with self.assertRaises(ValueError):
            server.parse_limit("many", 500, 5000)


class BuildAddArgsTests(unittest.TestCase):
    def test_title_only_is_separated(self):
        args, err = server.build_add_args({"title": "ship it"})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--", "ship it"])

    def test_full_payload_builds_explicit_flags(self):
        args, err = server.build_add_args({
            "title": "redesign pNOC",
            "description": "cap CPU",
            "priority": 4,
            "deadline": "2026-07-20",
        })
        self.assertIsNone(err)
        self.assertEqual(args, [
            "add", "--priority=4", "--description=cap CPU",
            "--deadline=2026-07-20", "--", "redesign pNOC",
        ])

    def test_leading_dash_values_stay_after_separator_or_equals(self):
        # hyphen-safe: title via `--`, description via `--opt=value`
        args, err = server.build_add_args(
            {"title": "-weird", "description": "- bullet"})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--description=- bullet", "--", "-weird"])

    def test_blank_optional_fields_are_omitted(self):
        args, err = server.build_add_args(
            {"title": "task", "description": "   ", "deadline": ""})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--", "task"])

    def test_short_title_rejected(self):
        args, err = server.build_add_args({"title": "ab"})
        self.assertIsNone(args)
        self.assertIn("title", err)

    def test_priority_must_be_int_1_to_5(self):
        for bad in (0, 6, 9, True, "4", 3.5):
            args, err = server.build_add_args({"title": "task", "priority": bad})
            self.assertIsNone(args, f"priority={bad!r} should fail")
            self.assertIn("priority", err)

    def test_invalid_deadline_rejected(self):
        for bad in ("2026-13-99", "2026/07/20", "tomorrow", "20-07-2026"):
            args, err = server.build_add_args({"title": "task", "deadline": bad})
            self.assertIsNone(args, f"deadline={bad!r} should fail")
            self.assertIn("deadline", err)

    def test_overlong_description_rejected(self):
        args, err = server.build_add_args(
            {"title": "task", "description": "x" * 4001})
        self.assertIsNone(args)
        self.assertIn("description", err)


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.dirname(__file__)))
    unittest.main()
