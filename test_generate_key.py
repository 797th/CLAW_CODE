import unittest

from generate_key import generate_key


class TestGenerateKey(unittest.TestCase):
    def test_random_salt_makes_new_keys_unique(self):
        key1 = generate_key("1990-01-31", "correct horse battery staple")
        key2 = generate_key("1990-01-31", "correct horse battery staple")

        self.assertNotEqual(key1, key2)
        self.assertTrue(key1.startswith("scrypt:"))
        self.assertTrue(key2.startswith("scrypt:"))

    def test_same_inputs_with_same_salt_are_deterministic(self):
        salt = bytes(range(16))

        key1 = generate_key("1990-01-31", "correct horse battery staple", salt=salt)
        key2 = generate_key("1990-01-31", "correct horse battery staple", salt=salt)

        self.assertEqual(key1, key2)

    def test_different_secret_changes_key(self):
        salt = bytes(range(16))

        key1 = generate_key("1990-01-31", "first secret", salt=salt)
        key2 = generate_key("1990-01-31", "second secret", salt=salt)

        self.assertNotEqual(key1, key2)

    def test_normalization(self):
        salt = bytes(range(16))
        secret = "correct horse battery staple"

        key1 = generate_key("19900131", secret, salt=salt)
        key2 = generate_key("1990-01-31", secret, salt=salt)
        key3 = generate_key("1990/01/31", secret, salt=salt)

        self.assertEqual(key1, key2)
        self.assertEqual(key2, key3)

    def test_invalid_input(self):
        with self.assertRaises(ValueError):
            generate_key("NoDigitsHere", "correct horse battery staple")
        with self.assertRaises(ValueError):
            generate_key("1990-01-31", "")
        with self.assertRaises(ValueError):
            generate_key("1990-01-31", "secret", salt=b"short")


if __name__ == "__main__":
    unittest.main()
