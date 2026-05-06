import base64
import hashlib
import secrets

KEY_BYTES = 32
SALT_BYTES = 16
SCRYPT_N = 2**14
SCRYPT_R = 8
SCRYPT_P = 1


def _normalize_birthday(birthday: str) -> str:
    normalized = "".join(filter(str.isdigit, birthday))
    if not normalized:
        raise ValueError("Birthday must contain at least one digit")
    return normalized


def _b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def generate_key(birthday: str, secret: str, *, salt: bytes | None = None) -> str:
    """Derive a 256-bit encryption key with scrypt.

    A birthday is low-entropy public data, so it is used only as context. The
    caller must provide a real secret, and each new key gets a random salt by
    default. Pass ``salt`` only for tests or when re-deriving a stored key.
    """
    normalized = _normalize_birthday(birthday)
    if not secret:
        raise ValueError("Secret must not be empty")
    if salt is None:
        salt = secrets.token_bytes(SALT_BYTES)
    if len(salt) < SALT_BYTES:
        raise ValueError(f"Salt must be at least {SALT_BYTES} bytes")

    password = f"{normalized}:{secret}".encode("utf-8")
    key = hashlib.scrypt(
        password,
        salt=salt,
        n=SCRYPT_N,
        r=SCRYPT_R,
        p=SCRYPT_P,
        dklen=KEY_BYTES,
    )
    return f"scrypt:{SCRYPT_N}:{SCRYPT_R}:{SCRYPT_P}:{_b64(salt)}:{_b64(key)}"


if __name__ == "__main__":
    import argparse
    import getpass

    parser = argparse.ArgumentParser(description="Generate a scrypt-derived encryption key")
    parser.add_argument("birthday", help="Birthday string (e.g., 1990-01-31)")
    parser.add_argument("--secret", help="High-entropy secret. Omit to enter it securely.")
    args = parser.parse_args()
    secret = args.secret if args.secret is not None else getpass.getpass("Secret: ")
    print(generate_key(args.birthday, secret))
