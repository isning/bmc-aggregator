"""Read-only device status probe. Uses credentials held by the running aggregator."""

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

MAX_RESPONSE_BYTES = 2 * 1024 * 1024
COMMON_FIELDS = ("available", "online", "state", "backend")


def request(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        body = response.read(MAX_RESPONSE_BYTES + 1)
    if len(body) > MAX_RESPONSE_BYTES:
        raise ValueError("status response exceeds size limit")
    document = json.loads(body)
    if not isinstance(document, dict):
        raise ValueError("status response must be an object")
    return document


def select(document, fields):
    return {field: document.get(field) for field in fields}


def probe(base_url, device_id):
    parsed = urllib.parse.urlsplit(base_url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("aggregator URL must use HTTP or HTTPS")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("aggregator URL must not contain credentials, query or fragment")
    base = f"{base_url.rstrip('/')}/bmc/{urllib.parse.quote(device_id, safe='')}/api"
    features = request(f"{base}/features")
    statuses = {
        feature: request(f"{base}/feature/{feature}/status")
        for feature in ("video", "hid", "power", "msd")
    }
    power = statuses["power"]
    return {
        "device_id": device_id,
        "registered_features": [
            select(feature, ("id", "backend"))
            for feature in features.get("features", [])
        ],
        "video": select(statuses["video"], COMMON_FIELDS + ("resolution", "fps")),
        "hid": select(statuses["hid"], COMMON_FIELDS),
        "power": {
            **select(power, COMMON_FIELDS + ("power_status",)),
            "has_error": power.get("fault") is not None or power.get("power_error") is not None,
        },
        "virtual_media": select(statuses["msd"], COMMON_FIELDS + ("connected",)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("device_id", help="configured aggregator device ID")
    parser.add_argument(
        "--url", default=os.environ.get("BMC_AGGREGATOR_URL", "http://127.0.0.1:8080")
    )
    args = parser.parse_args()
    try:
        report = probe(args.url, args.device_id)
    except urllib.error.HTTPError as error:
        print(f"status probe failed: HTTP {error.code}", file=sys.stderr)
        return 1
    except (OSError, ValueError, TypeError, KeyError) as error:
        # Responses and exception messages may contain credentials or remote payloads.
        print(f"status probe failed ({type(error).__name__})", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
