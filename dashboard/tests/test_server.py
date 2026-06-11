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
        self.assertEqual(server.VERSION, "0.1.3")
        self.assertGreater(server.MAX_POST_BYTES, 400)


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.dirname(__file__)))
    unittest.main()
