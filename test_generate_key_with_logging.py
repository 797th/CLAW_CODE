import json
import os
import unittest

from generate_key_with_logging import LOG_FILENAME, MEMORY_FILE, generate_key


class TestGenerateKeyWithLogging(unittest.TestCase):
    def setUp(self):
        self._cleanup()

    def tearDown(self):
        self._cleanup()

    def _cleanup(self):
        for file in (MEMORY_FILE, LOG_FILENAME):
            if os.path.exists(file):
                os.remove(file)

    def test_key_generation_and_memory_exclude_sensitive_values(self):
        birthday = "1990-01-31"
        secret = "correct horse battery staple"
        key = generate_key(birthday, secret, salt=bytes(range(16)))

        self.assertTrue(key.startswith("scrypt:"))
        self.assertTrue(os.path.exists(MEMORY_FILE))
        with open(MEMORY_FILE, "r", encoding="utf-8") as f:
            mem = json.load(f)

        entry = mem["last_generation"]
        self.assertEqual(entry["scheme"], "scrypt")
        self.assertEqual(entry["key_bits"], 256)
        self.assertIn("timestamp", entry)
        self.assertNotIn("birthday", entry)
        self.assertNotIn("key", entry)
        self.assertNotIn(birthday, json.dumps(mem))
        self.assertNotIn(key, json.dumps(mem))

    def test_dream_log_excludes_sensitive_values(self):
        birthday = "1991-02-28"
        secret = "correct horse battery staple"
        key = generate_key(birthday, secret, salt=bytes(range(16)))

        self.assertTrue(os.path.exists(LOG_FILENAME))
        with open(LOG_FILENAME, "r", encoding="utf-8") as f:
            logs = f.read()

        self.assertIn("Generated encryption key metadata", logs)
        self.assertIn("INFO", logs)
        self.assertNotIn(birthday, logs)
        self.assertNotIn(secret, logs)
        self.assertNotIn(key, logs)

    def test_invalid_input(self):
        with self.assertRaises(ValueError):
            generate_key("NoDigits", "correct horse battery staple")


if __name__ == "__main__":
    unittest.main()
