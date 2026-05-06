"""Generate a scrypt-derived encryption key and log only non-sensitive metadata."""

import json
import logging
import os
from datetime import datetime, timezone
from typing import Dict

from generate_key import generate_key as derive_key

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
LOG_FILENAME = os.path.join(BASE_DIR, "dream.log")
MEMORY_FILE = os.path.join(BASE_DIR, "memory_state.json")

LOGGER = logging.getLogger(__name__)
LOGGER.setLevel(logging.INFO)
LOGGER.propagate = False


def _write_log(level: int, message: str, *args: object) -> None:
    handler = logging.FileHandler(LOG_FILENAME, encoding="utf-8")
    handler.setFormatter(
        logging.Formatter(
            "%(asctime)s | %(levelname)s | %(message)s",
            datefmt="%Y-%m-%d %H:%M:%S",
        )
    )
    try:
        LOGGER.addHandler(handler)
        LOGGER.log(level, message, *args)
    finally:
        LOGGER.removeHandler(handler)
        handler.close()


def _load_memory() -> Dict:
    if os.path.exists(MEMORY_FILE):
        try:
            with open(MEMORY_FILE, "r", encoding="utf-8") as f:
                return json.load(f)
        except Exception as e:
            _write_log(logging.WARNING, "Failed to read memory state file: %s", e)
    return {}


def _save_memory(state: Dict) -> None:
    try:
        with open(MEMORY_FILE, "w", encoding="utf-8") as f:
            json.dump(state, f, indent=2, ensure_ascii=False)
    except Exception as e:
        _write_log(logging.ERROR, "Failed to write memory state file: %s", e)


def _utc_timestamp() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def generate_key(birthday: str, secret: str, *, salt: bytes | None = None) -> str:
    """Derive a key and persist only audit metadata.

    Birthday and key material are intentionally excluded from logs and memory.
    """
    key = derive_key(birthday, secret, salt=salt)
    _write_log(logging.INFO, "Generated encryption key metadata; sensitive values omitted")

    mem = _load_memory()
    mem["last_generation"] = {
        "timestamp": _utc_timestamp(),
        "scheme": key.split(":", 1)[0],
        "key_bits": 256,
    }
    _save_memory(mem)

    return key


if __name__ == "__main__":
    import argparse
    import getpass

    parser = argparse.ArgumentParser(
        description="Generate a scrypt-derived encryption key and log safe metadata."
    )
    parser.add_argument("birthday", help="Birthday string, e.g., 1990-01-31")
    parser.add_argument("--secret", help="High-entropy secret. Omit to enter it securely.")
    args = parser.parse_args()

    secret = args.secret if args.secret is not None else getpass.getpass("Secret: ")
    print(generate_key(args.birthday, secret))
