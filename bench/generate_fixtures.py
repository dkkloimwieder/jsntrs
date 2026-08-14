#!/usr/bin/env python3
"""Generate benchmark data fixtures for jsntrs.

Produces four fixture families, all deterministic — the same invocation always
writes the same bytes:

  account/  bench/data_*.json         (Account.Order.Product shapes, index-driven)
  logs/     bench/fixtures/logs/      (log records, seeded RNG)
  strings/  bench/fixtures/strings/   (string-manipulation payloads, seeded RNG)
  datetime/ bench/fixtures/datetime/  (timestamp payloads, seeded RNG)

Determinism notes
-----------------
* The account fixtures are pure functions of (order index, product index) —
  no RNG at all.
* The logs/strings/datetime fixtures share one `random.seed(SEED)` stream.
  To keep every file byte-stable no matter which subset was requested, the
  canonical stream is always replayed in a fixed order (CANONICAL_PLAN) and
  entries for unrequested files are generated and discarded.

Usage
-----
    python3 bench/generate_fixtures.py                       # everything missing
    python3 bench/generate_fixtures.py --only account,logs
    python3 bench/generate_fixtures.py --sizes 1k,10k
    python3 bench/generate_fixtures.py --force               # rewrite existing
    python3 bench/generate_fixtures.py --check               # verify sha256s
    python3 bench/generate_fixtures.py --write-manifest      # refresh sha256s

Output paths are always resolved relative to this script's directory, never
the current working directory.
"""

import argparse
import base64
import hashlib
import json
import os
import random
import sys
from urllib.parse import quote as url_quote
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
FIXTURES_DIR = BENCH_DIR / "fixtures"
MANIFEST = BENCH_DIR / "fixtures.sha256"

SEED = 42
SIZES = ("1k", "10k", "100k")
TYPES = ("account", "logs", "strings", "datetime")
RNG_TYPES = ("logs", "strings", "datetime")

# ── Account fixtures ─────────────────────────────────────────────────────────
#
# Shape (all flavours):
#   {"Account": {"Name": "Firefly", "Order": [{"OrderID": ..., "Product": [...]}]}}
#
# Three flavours:
#   short  — compact key names, ramping prices (also the tracked data_1k.json)
#   long   — verbose key names, prices spread over [10, 100)
#   mixed  — short key *names* with values alternating short/long per product,
#            so short-key benchmark expressions still apply while the payload
#            carries long-key-sized strings.

# (filename, orders, products per order, flavour)
ACCOUNT_SPECS = {
    "1k": [
        ("data_1k.json", 10, 100, "short"),
        ("data_1k_long.json", 100, 10, "long"),
    ],
    "10k": [
        ("data_10k.json", 100, 100, "short"),
        ("data_10k_long.json", 1000, 10, "long"),
        ("data_10k_mixed.json", 1000, 10, "mixed"),
    ],
    "100k": [
        ("data_100k.json", 200, 500, "short"),
        ("data_100k_long.json", 10000, 10, "long"),
        ("data_100k_mixed.json", 10000, 10, "mixed"),
    ],
}

ACCOUNT_NAME = "Firefly"

SHORT_META = {"Color": "Black", "Size": "M", "Material": "Felt", "Origin": "UK"}

# Short-value vocabulary used by the mixed flavour (the pure-short fixtures use
# the constant SHORT_META above, matching the tracked data_1k.json).
MIXED_SHORT_COLORS = ["Black", "Red", "Green", "White", "Blue"]
MIXED_SHORT_TRAITS = [("S", "Felt", "UK"), ("L", "Wool", "DE")]

LONG_COLORS = [
    "MidnightBlueWithShimmer",
    "ElectricCrimsonHighlight",
    "ForestGreenWithGoldTrim",
    "PlatinumSilverBrushed",
    "DeepOceanTealMatte",
]
LONG_TRAITS = [
    ("ExtraSmallCompressed", "ReinforcedCarbonFiberComposite", "UnitedKingdomOfGreatBritain"),
    ("MediumStandardFitting", "OrganicBambooBlendFabric", "UnitedStatesOfAmericaWest"),
    ("LargeOversizedRelaxed", "RecycledPolyesterMicroweave", "FederalRepublicOfGermany"),
    ("ExtraLargeExtendedFit", "SustainableHempCottonMix", "JapanTokyoMetropolitan"),
]


def short_price(i: int, j: int) -> float:
    """Ramping price; spans well past 50 for every fixture size."""
    return round(10 + 7.5 * j + 0.1 * i, 2)


def long_price(i: int, j: int) -> float:
    """Price spread over [10, 100) so `> 50` filters keep both branches full."""
    return round(10 + (i * 6.7 + j * 3.1) % 90, 2)


def short_product(i: int, j: int) -> dict:
    return {
        "SKU": f"SKU-{i:04d}-{j:04d}",
        "Description": f"Product {j} in order {i}",
        "UnitPrice": short_price(i, j),
        "Quantity": (j % 5) + 1,
        "Discount": round((j % 4) * 0.05, 2),
        "Metadata": dict(SHORT_META),
    }


def long_product(i: int, j: int) -> dict:
    color = LONG_COLORS[(i + j) % len(LONG_COLORS)]
    size, material, origin = LONG_TRAITS[(i + j) % len(LONG_TRAITS)]
    return {
        "StockKeepingUnitIdentifier": f"SKU-EXTENDED-PRODUCT-{i:04d}-{j:04d}",
        "ProductDescriptionExtended": (
            "This is a comprehensive extended product description for item "
            f"number {j} within purchase order {i}"
        ),
        "UnitPriceWithTaxIncluded": long_price(i, j),
        "QuantityInWarehouseStock": (j % 10) + 1,
        "ApplicableDiscountPercentage": round((j % 5) * 0.05, 2),
        "ExtendedMetadataInformation": {
            "ProductColorDescription": color,
            "ProductSizeClassification": size,
            "MaterialCompositionDetails": material,
            "CountryOfOriginInformation": origin,
        },
    }


def mixed_product(i: int, j: int) -> dict:
    """Short key names; values alternate short/long by (order + product) parity."""
    k = i + j
    if k % 2 == 0:
        sku = f"SKU-{i:04d}-{j:04d}"
        desc = f"Product {j} in order {i}"
        color = MIXED_SHORT_COLORS[(k // 2) % len(MIXED_SHORT_COLORS)]
        size, material, origin = MIXED_SHORT_TRAITS[(k // 2) % len(MIXED_SHORT_TRAITS)]
    else:
        sku = f"SKU-EXTENDED-PRODUCT-{i:04d}-{j:04d}"
        desc = (
            "This is a comprehensive extended product description for item "
            f"number {j} within purchase order {i}"
        )
        color = LONG_COLORS[(k // 2) % len(LONG_COLORS)]
        size, material, origin = LONG_TRAITS[(k // 2) % len(LONG_TRAITS)]
    return {
        "SKU": sku,
        "Description": desc,
        "UnitPrice": long_price(i, j),
        "Quantity": (j % 10) + 1,
        "Discount": round((j % 5) * 0.05, 2),
        "Metadata": {
            "Color": color,
            "Size": size,
            "Material": material,
            "Origin": origin,
        },
    }


# flavour -> (product builder, price key, discount key, metadata key, metadata subkeys)
FLAVOURS = {
    "short": (
        short_product,
        "UnitPrice",
        "Discount",
        "Metadata",
        ("Color", "Size", "Material", "Origin"),
    ),
    "long": (
        long_product,
        "UnitPriceWithTaxIncluded",
        "ApplicableDiscountPercentage",
        "ExtendedMetadataInformation",
        (
            "ProductColorDescription",
            "ProductSizeClassification",
            "MaterialCompositionDetails",
            "CountryOfOriginInformation",
        ),
    ),
    "mixed": (
        mixed_product,
        "UnitPrice",
        "Discount",
        "Metadata",
        ("Color", "Size", "Material", "Origin"),
    ),
}


def check_order(order: dict, price_key: str, disc_key: str, meta_key: str, meta_subkeys) -> tuple:
    """Assert the per-order benchmark invariants; return (#>50, #<=50)."""
    assert order.get("OrderID"), "order is missing OrderID"
    assert order["OrderID"].startswith("order-"), f"bad OrderID {order['OrderID']!r}"
    products = order.get("Product")
    assert isinstance(products, list) and products, "order has no Product array"

    above = below = 0
    for p in products:
        price = p.get(price_key)
        assert isinstance(price, (int, float)), f"{price_key} not numeric: {price!r}"
        if price > 50:
            above += 1
        else:
            below += 1
        disc = p.get(disc_key)
        assert isinstance(disc, (int, float)) and 0 <= disc < 1, f"{disc_key} out of [0,1): {disc!r}"
        meta = p.get(meta_key)
        assert isinstance(meta, dict), f"missing {meta_key} object"
        for sk in meta_subkeys:
            assert sk in meta, f"{meta_key} missing {sk}"
    return above, below


def write_account_fixture(name: str, orders: int, products: int, flavour: str) -> Path:
    """Stream one account fixture to disk (streaming keeps 100k_long off the heap).

    The byte output is identical to `json.dump({...})` on the whole document:
    default separators mean the order array is exactly ", ".join(json.dumps(o)).
    """
    build, price_key, disc_key, meta_key, meta_subkeys = FLAVOURS[flavour]
    assert orders >= 2, f"{name}: need >= 2 orders, got {orders}"

    out = BENCH_DIR / name
    tmp = out.with_suffix(out.suffix + ".tmp")
    above = below = 0
    with open(tmp, "w") as f:
        f.write('{"Account": {"Name": ' + json.dumps(ACCOUNT_NAME) + ', "Order": [')
        for i in range(orders):
            order = {
                "OrderID": f"order-{i:04d}",
                "Product": [build(i, j) for j in range(products)],
            }
            a, b = check_order(order, price_key, disc_key, meta_key, meta_subkeys)
            above += a
            below += b
            if i:
                f.write(", ")
            f.write(json.dumps(order))
        f.write("]}}")

    assert above > 0, f"{name}: no product with {price_key} > 50"
    assert below > 0, f"{name}: no product with {price_key} <= 50"
    tmp.replace(out)
    return out


# ── Vocabulary for the RNG-driven fixtures ───────────────────────────────────

LOG_LEVELS = ["debug", "info", "info", "info", "warn", "warn", "error", "error"]
SERVICES = ["auth-svc", "api-gateway", "order-svc", "inventory-svc", "payment-svc", "notification-svc"]
LOG_MESSAGES = [
    "Connection timeout after {ms}ms to {host}",
    "Request completed successfully in {ms}ms",
    "Failed to authenticate user {user}: invalid credentials",
    "Cache miss for key {key}, fetching from database",
    "Rate limit exceeded for client {client}, throttling",
    "Health check passed: {service} (latency={ms}ms)",
    "Database query took {ms}ms: SELECT * FROM orders WHERE status = 'pending'",
    "Incoming request: {method} {path} from {ip}",
    "Response sent: {status} in {ms}ms",
    "Circuit breaker tripped for {service} after {count} failures",
    "Deploying version {version} to {env} environment",
    "Garbage collection completed in {ms}ms, freed {mb}MB",
    "SSL certificate for {domain} expires in {days} days",
    "Outbound webhook delivery failed: {url} returned {status}",
    "Message published to queue {queue}: {bytes} bytes",
]
HOSTS = ["db-primary.internal", "redis-01.cache", "kafka-broker-1", "postgres-replica.internal"]
METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH"]
PATHS = ["/api/v2/users", "/api/v2/orders", "/api/v2/inventory", "/api/v2/auth/token", "/health"]
TAGS_POOL = ["timeout", "database", "auth", "cache", "rate-limit", "deploy", "gc", "ssl", "webhook", "queue"]
REGIONS = ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-1"]

STRING_TEMPLATES = [
    "ERROR: Connection refused to {host} (host={fqdn}, port={port})",
    "WARNING: Slow query detected ({ms}ms): SELECT orders.* FROM orders JOIN products ON ...",
    "INFO: User {user} logged in from {ip} via {method}",
    "DEBUG: Cache hit ratio: {pct}% ({hits}/{total} requests)",
    "CRITICAL: Disk usage at {pct}% on volume /dev/sda1 ({gb}GB free)",
    "NOTICE: Scheduled maintenance window starting at {time} UTC",
    "ERROR: Invalid JSON payload in request body: unexpected token at position {pos}",
    "WARNING: Memory usage exceeds threshold: {mb}MB / {limit}MB ({pct}%)",
    "INFO: Batch job completed: processed {count} records in {ms}ms",
    "ERROR: TLS handshake failed with {host}: certificate verification error",
]


def size_to_count(size: str) -> int:
    return {"1k": 1000, "10k": 10000, "100k": 100000}[size]


def rand_iso_timestamp() -> str:
    year = 2026
    month = random.randint(1, 12)
    day = random.randint(1, 28)
    hour = random.randint(0, 23)
    minute = random.randint(0, 59)
    second = random.randint(0, 59)
    ms = random.randint(0, 999)
    return f"{year}-{month:02d}-{day:02d}T{hour:02d}:{minute:02d}:{second:02d}.{ms:03d}Z"


def rand_epoch_ms() -> int:
    base = 1735689600000  # 2025-01-01 00:00:00 UTC
    return base + random.randint(0, 365 * 24 * 3600 * 1000)


def generate_log_entry(i: int) -> dict:
    level = random.choice(LOG_LEVELS)
    service = random.choice(SERVICES)
    template = random.choice(LOG_MESSAGES)
    ms = random.randint(1, 30000)
    msg = template.format(
        ms=ms, host=random.choice(HOSTS), user=f"usr-{random.randint(1000,9999)}",
        key=f"cache:{random.randint(1,1000)}", client=f"client-{random.randint(1,50)}",
        service=service, method=random.choice(METHODS), path=random.choice(PATHS),
        ip=f"{random.randint(10,255)}.{random.randint(0,255)}.{random.randint(0,255)}.{random.randint(1,254)}",
        status=random.choice([200, 201, 400, 401, 403, 404, 500, 502, 503]),
        count=random.randint(1, 100), version=f"v{random.randint(1,5)}.{random.randint(0,20)}.{random.randint(0,99)}",
        env=random.choice(["production", "staging", "canary"]),
        mb=random.randint(50, 500), domain=f"api-{random.randint(1,5)}.example.com",
        days=random.randint(1, 365), url=f"https://webhook.example.com/endpoint-{random.randint(1,20)}",
        bytes=random.randint(100, 10000), queue=random.choice(["orders", "notifications", "analytics"]),
    )
    tags = random.sample(TAGS_POOL, k=random.randint(1, 3))
    return {
        "timestamp": rand_iso_timestamp(),
        "level": level,
        "service": service,
        "message": msg,
        "requestId": f"req-{random.randint(100000, 999999):06x}",
        "duration_ms": ms,
        "tags": tags,
        "context": {
            "host": random.choice(HOSTS),
            "port": random.choice([5432, 6379, 8080, 8443, 9092]),
            "region": random.choice(REGIONS),
        },
    }


def generate_string_entry(i: int) -> dict:
    template = random.choice(STRING_TEMPLATES)
    text = template.format(
        host=random.choice(HOSTS), fqdn=f"prod-{random.randint(1,20):02d}.internal",
        port=random.choice([5432, 6379, 8080, 8443, 9092]),
        ms=random.randint(1, 30000), user=f"user-{random.randint(1000,9999)}",
        ip=f"{random.randint(10,255)}.{random.randint(0,255)}.{random.randint(0,255)}.{random.randint(1,254)}",
        method=random.choice(METHODS), pct=random.randint(50, 99),
        hits=random.randint(500, 9000), total=random.randint(9000, 10000),
        gb=random.randint(1, 500), time=f"{random.randint(0,23):02d}:{random.randint(0,59):02d}",
        pos=random.randint(1, 1000), mb=random.randint(256, 4096),
        limit=random.randint(4096, 8192), count=random.randint(100, 100000),
    )
    code = f"ERR-{random.randint(1000, 9999)}"
    url = f"https://api.example.com/v{random.randint(1,3)}/{random.choice(['users', 'orders', 'products'])}?id={random.randint(1,10000)}&status={random.choice(['active', 'pending'])}"
    b64 = base64.b64encode(text[:50].encode()).decode()
    urlencoded = url_quote(text[:40])
    urlencoded_full = url_quote(url, safe="")

    return {
        "text": text,
        "code": code,
        "url": url,
        "b64": b64,
        "urlencoded": urlencoded,
        "urlencoded_full": urlencoded_full,
    }


def generate_datetime_entry(i: int) -> dict:
    ts = rand_iso_timestamp()
    epoch = rand_epoch_ms()
    months = ["January", "February", "March", "April", "May", "June",
              "July", "August", "September", "October", "November", "December"]
    m = random.randint(1, 12)
    d = random.randint(1, 28)
    return {
        "timestamp": ts,
        "epoch_ms": epoch,
        "formatted": f"{months[m-1]} {d}, 2026",
    }


RNG_GENERATORS = {
    "logs": generate_log_entry,
    "strings": generate_string_entry,
    "datetime": generate_datetime_entry,
}
WRAPPER_KEYS = {"logs": "entries", "strings": "items", "datetime": "events"}

# Fixed replay order of the shared RNG stream. Never reorder: it decides the
# bytes of every logs/strings/datetime fixture. Size-major, so the 1k prefix
# reproduces the tracked fixtures/{logs,strings,datetime}/1k.json byte for byte.
CANONICAL_PLAN = [(t, s) for s in SIZES for t in RNG_TYPES]


def rng_fixture_path(fixture_type: str, size: str) -> Path:
    return FIXTURES_DIR / fixture_type / f"{size}.json"


def write_rng_fixture(fixture_type: str, size: str, entries: list) -> Path:
    out = rng_fixture_path(fixture_type, size)
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(out.suffix + ".tmp")
    with open(tmp, "w") as f:
        json.dump({WRAPPER_KEYS[fixture_type]: entries}, f, separators=(",", ":"))
    tmp.replace(out)
    return out


# ── Symlinks ─────────────────────────────────────────────────────────────────

ACCOUNT_LINKS = {
    "tiny.json": "../../data.json",
    "1k.json": "../../data_1k.json",
    "10k.json": "../../data_10k.json",
    "100k.json": "../../data_100k.json",
    "10k_long.json": "../../data_10k_long.json",
    "100k_long.json": "../../data_100k_long.json",
}


def setup_account_symlinks() -> None:
    """Recreate bench/fixtures/account/*.json -> bench/data_*.json.

    Links may dangle when the large fixtures have not been generated; that is
    intended and harmless.
    """
    account_dir = FIXTURES_DIR / "account"
    account_dir.mkdir(parents=True, exist_ok=True)
    for name, target in ACCOUNT_LINKS.items():
        link = account_dir / name
        if link.is_symlink() and os.readlink(link) == target:
            continue
        if link.exists() or link.is_symlink():
            link.unlink()
        link.symlink_to(target)
        print(f"  linked fixtures/account/{name} -> {target}")


# ── Manifest ─────────────────────────────────────────────────────────────────

def all_generated_paths() -> list:
    """Every file this script can produce, in manifest order."""
    paths = []
    for size in SIZES:
        for name, _o, _p, _f in ACCOUNT_SPECS[size]:
            paths.append(BENCH_DIR / name)
    for fixture_type in RNG_TYPES:
        for size in SIZES:
            paths.append(rng_fixture_path(fixture_type, size))
    return paths


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def rel(path: Path) -> str:
    return str(path.relative_to(BENCH_DIR))


def write_manifest() -> int:
    lines = []
    for path in all_generated_paths():
        if path.exists():
            lines.append(f"{sha256_file(path)}  {rel(path)}\n")
        else:
            print(f"  skip (not generated): {rel(path)}", file=sys.stderr)
    MANIFEST.write_text("".join(lines))
    print(f"Wrote {rel(MANIFEST)} ({len(lines)} entries)")
    return 0


def check_manifest(wanted: set) -> int:
    if not MANIFEST.exists():
        print(f"error: {rel(MANIFEST)} not found", file=sys.stderr)
        return 1

    expected = {}
    for lineno, line in enumerate(MANIFEST.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        digest, _, name = line.partition("  ")
        if not name:
            print(f"error: {rel(MANIFEST)}:{lineno}: malformed line", file=sys.stderr)
            return 1
        expected[name.strip()] = digest

    ok = missing = bad = 0
    for path in all_generated_paths():
        name = rel(path)
        if name not in expected:
            continue
        if wanted is not None and name not in wanted:
            continue
        if not path.exists():
            print(f"  MISSING  {name}")
            missing += 1
            continue
        actual = sha256_file(path)
        if actual == expected[name]:
            print(f"  OK       {name}")
            ok += 1
        else:
            print(f"  MISMATCH {name}\n           expected {expected[name]}\n           actual   {actual}")
            bad += 1

    print(f"\n{ok} ok, {bad} mismatched, {missing} not generated")
    if bad:
        return 1
    if ok == 0:
        print("error: nothing verified — generate the fixtures first", file=sys.stderr)
        return 1
    return 0


# ── Reporting ────────────────────────────────────────────────────────────────

def human(nbytes: int) -> str:
    if nbytes >= 1 << 20:
        return f"{nbytes / (1 << 20):.1f}MB"
    if nbytes >= 1 << 10:
        return f"{nbytes / (1 << 10):.0f}KB"
    return f"{nbytes}B"


def report(path: Path, detail: str) -> None:
    print(f"  wrote {rel(path):<28} {human(path.stat().st_size):>8}  ({detail})")


def skipped(path: Path, detail: str) -> None:
    print(f"  keep  {rel(path):<28} {human(path.stat().st_size):>8}  ({detail}; --force to rewrite)")


# ── Main ─────────────────────────────────────────────────────────────────────

def parse_csv(value: str, allowed, what: str) -> list:
    items = [v.strip() for v in value.split(",") if v.strip()]
    for item in items:
        if item not in allowed:
            raise SystemExit(f"error: unknown {what} {item!r} (choose from {', '.join(allowed)})")
    return items


def main() -> int:
    p = argparse.ArgumentParser(description="Generate benchmark fixtures")
    p.add_argument("--only", default=",".join(TYPES),
                   help=f"fixture types to generate (comma-separated: {','.join(TYPES)})")
    p.add_argument("--sizes", default=",".join(SIZES),
                   help=f"sizes to generate (comma-separated: {','.join(SIZES)})")
    p.add_argument("--force", action="store_true", help="rewrite fixtures that already exist")
    p.add_argument("--check", action="store_true",
                   help=f"verify sha256 of generated fixtures against {MANIFEST.name} and exit")
    p.add_argument("--write-manifest", action="store_true",
                   help=f"(re)write {MANIFEST.name} from the fixtures on disk and exit")
    args = p.parse_args()

    types = parse_csv(args.only, TYPES, "fixture type")
    sizes = parse_csv(args.sizes, SIZES, "size")

    if args.check:
        wanted = set()
        for size in sizes:
            if "account" in types:
                wanted.update(name for name, _o, _p, _f in ACCOUNT_SPECS[size])
            for fixture_type in types:
                if fixture_type in RNG_TYPES:
                    wanted.add(rel(rng_fixture_path(fixture_type, size)))
        return check_manifest(wanted)

    if args.write_manifest:
        return write_manifest()

    print("Symlinks:")
    setup_account_symlinks()

    if "account" in types:
        print("\nAccount fixtures:")
        for size in sizes:
            for name, orders, products, flavour in ACCOUNT_SPECS[size]:
                detail = f"{orders} orders x {products} products, {flavour} keys"
                out = BENCH_DIR / name
                if out.exists() and not args.force:
                    skipped(out, detail)
                    continue
                report(write_account_fixture(name, orders, products, flavour), detail)

    wanted = {(t, s) for t in types if t in RNG_TYPES for s in sizes}
    if wanted:
        print("\nLog / string / datetime fixtures:")
        # Replay the shared RNG stream in canonical order so each file's bytes
        # depend only on its position in the plan, not on what was requested.
        last = max(idx for idx, ts in enumerate(CANONICAL_PLAN) if ts in wanted)
        random.seed(SEED)
        for idx, (fixture_type, size) in enumerate(CANONICAL_PLAN):
            count = size_to_count(size)
            gen = RNG_GENERATORS[fixture_type]
            out = rng_fixture_path(fixture_type, size)
            write = (fixture_type, size) in wanted and (args.force or not out.exists())
            if write:
                report(write_rng_fixture(fixture_type, size, [gen(i) for i in range(count)]),
                       f"{count} entries")
            else:
                # Still advance the stream; only the bytes are discarded.
                for i in range(count):
                    gen(i)
                if (fixture_type, size) in wanted:
                    skipped(out, f"{count} entries")
            if idx == last:
                break

    print("\nDone.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
