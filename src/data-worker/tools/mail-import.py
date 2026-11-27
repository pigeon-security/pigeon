#!/usr/bin/env python3
"""Read-only IMAP backfill into an opted-in instance Data Worker."""

import argparse
import datetime
import getpass
import hashlib
import hmac
import imaplib
import json
import os
import re
import socket
import sqlite3
import ssl
import struct
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

PATH = "/data/mail-import"
MAX_BYTES = 25 * 1024 * 1024
UID_WINDOW = 1024
PROVIDERS = {
    "gmail": ("imap.gmail.com", 993),
    "icloud": ("imap.mail.me.com", 993),
    "fastmail": ("imap.fastmail.com", 993),
    "proton-bridge": ("127.0.0.1", 1143),
}
UID = re.compile(rb"\bUID (\d+)\b")
SIZE = re.compile(rb"\bRFC822.SIZE (\d+)\b")
LIST_ENTRY = re.compile(
    rb'^\(([^)]*)\) (?:NIL|"(?:[^"\\]|\\.)*") (.+)$', re.IGNORECASE
)


class ImportErrorSafe(Exception):
    """Failure text suitable for an audit receipt."""


class Retryable(ImportErrorSafe):
    """An interrupted connection can resume from its last committed UID."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def target(url):
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in ("", "/")
        or parsed.query
        or parsed.fragment
    ):
        raise ImportErrorSafe("destination must be an HTTPS origin")
    return urllib.parse.urlunsplit(("https", parsed.netloc, PATH, "", ""))


def quote(value):
    if not value or any(ord(c) < 32 or ord(c) > 126 for c in value):
        raise ImportErrorSafe("IMAP identifiers must use plain ASCII or modified UTF-7")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def mailboxes(client):
    kind, entries = client.list()
    if kind != "OK":
        raise Retryable("IMAP mailbox listing was rejected")
    for entry in entries:
        if not entry:
            continue
        line, literal = entry if isinstance(entry, tuple) else (entry, None)
        match = LIST_ENTRY.fullmatch(line)
        if not match:
            raise ImportErrorSafe("could not parse IMAP mailbox listing")
        flags, name = match.groups()
        if b"\\noselect" in flags.lower().split():
            continue
        if literal is not None:
            name = literal
        elif name.startswith(b'"') and name.endswith(b'"'):
            name = re.sub(rb"\\(.)", rb"\1", name[1:-1])
        try:
            yield name.decode("ascii")
        except UnicodeError:
            raise ImportErrorSafe("IMAP mailbox name was not modified UTF-7") from None


def connect(args, password):
    host, port = PROVIDERS[args.provider]
    if args.provider == "proton-bridge":
        context = ssl.create_default_context(cafile=args.bridge_certificate)
        # The supplied Bridge certificate is the trust anchor. Bridge often uses localhost in
        # the certificate even though this socket is pinned to 127.0.0.1.
        context.check_hostname = False
        client = imaplib.IMAP4(host, args.bridge_port, timeout=45)
        try:
            client.starttls(context)
        except BaseException:
            client.shutdown()
            raise
    else:
        context = ssl.create_default_context()
        client = imaplib.IMAP4_SSL(host, port, ssl_context=context, timeout=45)
    try:
        client.login(quote(args.account), password)
    except imaplib.IMAP4.abort:
        client.shutdown()
        raise Retryable("IMAP connection interrupted during login") from None
    except imaplib.IMAP4.error:
        client.shutdown()
        raise ImportErrorSafe("IMAP authentication failed") from None
    return client


def server_number(client, name):
    _, values = client.response(name)
    if (
        not values
        or not isinstance(values[-1], bytes)
        or not values[-1].isdigit()
        or not 0 < int(values[-1]) <= 0xFFFFFFFF
    ):
        raise Retryable(f"IMAP did not report {name}")
    return int(values[-1])


def fetch(client, uid, fields):
    result, rows = client.uid("FETCH", str(uid), fields)
    if result != "OK":
        raise Retryable("IMAP fetch was rejected")
    for index, row in enumerate(rows):
        if not row:
            continue
        header, raw = row if isinstance(row, tuple) else (row, None)
        if raw is not None and index + 1 < len(rows):
            trailer = rows[index + 1]
            if isinstance(trailer, bytes) and trailer.startswith((b" ", b")")):
                header += trailer
        found = UID.search(header)
        if found and int(found[1]) == uid:
            return header, raw
    raise Retryable("IMAP message disappeared after listing")


def message_bytes(client, uid):
    header, _ = fetch(client, uid, "(UID RFC822.SIZE)")
    size = SIZE.search(header)
    if size is None:
        raise Retryable("IMAP did not report the message size")
    if int(size[1]) > MAX_BYTES:
        raise ImportErrorSafe("message exceeds the Data Worker import limit")
    # Request a bounded literal even if a server reports an incorrect message length.
    _, raw = fetch(client, uid, f"(UID BODY.PEEK[]<0.{MAX_BYTES + 1}>)")
    if not isinstance(raw, bytes) or len(raw) != int(size[1]) or not raw:
        raise Retryable("IMAP returned an incomplete message")
    return raw


def upload(opener, endpoint, key, source, raw):
    metadata = json.dumps(source, ensure_ascii=True, separators=(",", ":")).encode()
    if len(metadata) > 8192:
        raise ImportErrorSafe("IMAP message metadata exceeds the import limit")
    body = struct.pack(">I", len(metadata)) + metadata + raw
    timestamp = time.time_ns() // 1_000_000
    digest = hashlib.sha256(body).hexdigest()
    message = f"POST\n{PATH}\n{timestamp}\n{digest}".encode()
    signature = hmac.digest(key, message, "sha256").hex()
    request = urllib.request.Request(
        endpoint,
        body,
        {
            "Content-Type": "application/octet-stream",
            "X-Pigeon-Body-SHA256": digest,
            "X-Pigeon-Signature": f"t={timestamp},v1={signature}",
        },
        method="POST",
    )
    try:
        with opener.open(request, timeout=90) as response:
            result = json.loads(response.read(4096))
    except urllib.error.HTTPError as error:
        if error.code in (408, 425, 429) or error.code >= 500:
            raise Retryable("Data Worker temporarily rejected the import") from None
        raise ImportErrorSafe(f"Data Worker rejected import (HTTP {error.code})") from None
    except (urllib.error.URLError, TimeoutError):
        raise Retryable("Data Worker import connection interrupted") from None
    except ValueError:
        raise Retryable("Data Worker returned an invalid import receipt") from None
    if not isinstance(result, dict) or (
        result.get("id") != hashlib.sha256(raw).hexdigest()
        or not isinstance(result.get("duplicate"), bool)
    ):
        raise Retryable("Data Worker returned an invalid import receipt")
    return result["duplicate"]


def sync_mailbox(client, mailbox, args, state, opener, key, endpoint, counts):
    if client.select(quote(mailbox), readonly=True)[0] != "OK":
        raise Retryable("IMAP read-only mailbox selection was rejected")
    validity = server_number(client, "UIDVALIDITY")
    ceiling = server_number(client, "UIDNEXT") - 1
    scope = (endpoint, args.provider, args.account, mailbox)
    row = state.execute(
        """SELECT uid_validity, uid FROM cursors
        WHERE endpoint=? AND provider=? AND account=? AND mailbox=?""",
        scope,
    ).fetchone()
    cursor = row[1] if row and row[0] == validity else 0

    def checkpoint(uid):
        state.execute(
            "INSERT OR REPLACE INTO cursors VALUES (?, ?, ?, ?, ?, ?)",
            (*scope, validity, uid),
        )
        state.commit()

    while cursor < ceiling and counts["messages"] < args.batch:
        upper = min(ceiling, cursor + UID_WINDOW)
        result, rows = client.uid("SEARCH", None, "UID", f"{cursor + 1}:{upper}")
        if result != "OK":
            raise Retryable("IMAP UID search was rejected")
        tokens = rows[0].split() if rows and rows[0] else []
        if any(not u.isdigit() or not cursor < int(u) <= upper for u in tokens):
            raise Retryable("IMAP search returned a UID outside its requested range")
        for uid in sorted(set(map(int, tokens))):
            if counts["messages"] >= args.batch:
                return
            source = {
                "kind": "imap",
                "server": "proton-bridge" if args.provider == "proton-bridge"
                else PROVIDERS[args.provider][0],
                "account": args.account,
                "mailbox": mailbox,
                "uid_validity": validity,
                "uid": uid,
            }
            raw = message_bytes(client, uid)
            duplicate = upload(opener, endpoint, key, source, raw)
            checkpoint(uid)
            counts["messages"] += 1
            counts["duplicates"] += int(duplicate)
            counts["bytes"] += len(raw)
            time.sleep(args.interval)
        checkpoint(upper)
        cursor = upper


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def run(args, counts):
    endpoint = target(args.destination)
    password = os.environ.get("PIGEON_IMAP_PASSWORD") or getpass.getpass("IMAP password: ")
    key = (os.environ.get("PIGEON_MAIL_IMPORT_KEY") or getpass.getpass("Mail import key: ")).encode()
    if len(key.strip()) < 32:
        raise ImportErrorSafe("mail import key must contain at least 32 bytes")
    opener = urllib.request.build_opener(NoRedirect())
    with sqlite3.connect(args.state) as state:
        # One importer owns its checkpoints for the duration of the batch.
        state.execute("PRAGMA locking_mode=EXCLUSIVE")
        state.execute("BEGIN EXCLUSIVE")
        state.execute("""CREATE TABLE IF NOT EXISTS cursors (
            endpoint TEXT, provider TEXT, account TEXT, mailbox TEXT, uid_validity INTEGER,
            uid INTEGER, PRIMARY KEY (endpoint, provider, account, mailbox))""")
        state.commit()
        for attempt in range(args.attempts):
            client = None
            try:
                client = connect(args, password)
                selected = args.mailbox or list(mailboxes(client))
                for mailbox in selected:
                    if counts["messages"] >= args.batch:
                        break
                    sync_mailbox(client, mailbox, args, state, opener, key, endpoint, counts)
                return
            except (Retryable, imaplib.IMAP4.abort, socket.timeout, ConnectionError):
                if attempt + 1 == args.attempts:
                    raise ImportErrorSafe("import interrupted after retry limit") from None
                time.sleep(min(300, 5 * (2 ** attempt)))
            except imaplib.IMAP4.error:
                raise ImportErrorSafe("IMAP server rejected a read-only operation") from None
            finally:
                if client is not None:
                    try:
                        client.logout()
                    except (OSError, imaplib.IMAP4.error):
                        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider", choices=PROVIDERS)
    parser.add_argument("destination", help="instance Data Worker HTTPS origin")
    parser.add_argument("account", help="IMAP login name; for Proton use Bridge-generated credentials")
    parser.add_argument("--mailbox", action="append", help="IMAP folder (repeat); default all selectable")
    parser.add_argument("--bridge-certificate", type=Path, help="trusted local Bridge certificate")
    parser.add_argument("--bridge-port", type=int, default=1143)
    parser.add_argument("--batch", type=int, default=200)
    parser.add_argument("--interval", type=float, default=0.25)
    parser.add_argument("--attempts", type=int, default=4)
    parser.add_argument("--state", type=Path, default=Path("mail-import.sqlite3"))
    parser.add_argument("--audit", type=Path, default=Path("mail-import-audit.jsonl"))
    args = parser.parse_args()
    if args.provider == "proton-bridge" and args.bridge_certificate is None:
        parser.error("Proton Bridge requires --bridge-certificate")
    if not (1 <= args.batch <= 10000 and 0.1 <= args.interval <= 10 and 1 <= args.attempts <= 8):
        parser.error("batch, interval or attempt limit out of range")
    if not 1 <= args.bridge_port <= 65535:
        parser.error("Bridge port out of range")
    os.umask(0o077)
    run_id = uuid.uuid4().hex
    receipt = {
        "kind": "email_ingest",
        "run_id": run_id,
        "provider": args.provider,
        "started_at": now(),
        "counts": {"messages": 0, "duplicates": 0, "bytes": 0},
    }
    try:
        run(args, receipt["counts"])
        receipt["outcome"] = "complete_batch"
    except ImportErrorSafe as error:
        receipt.update(outcome="failed", reason=str(error))
    except KeyboardInterrupt:
        receipt.update(outcome="stopped", reason="operator interrupted import")
    except (OSError, sqlite3.Error, ValueError):
        receipt.update(outcome="failed", reason="could not access connection or local state")
    receipt["finished_at"] = now()
    with args.audit.open("a", encoding="utf-8") as audit:
        audit.write(json.dumps(receipt) + "\n")
        audit.flush()
        os.fsync(audit.fileno())
    print(json.dumps(receipt))
    return 0 if receipt["outcome"] == "complete_batch" else 1


if __name__ == "__main__":
    raise SystemExit(main())
