#!/usr/bin/env python3
"""Embed the public updater contract before signing an app. Never handles secrets."""
import argparse
import base64
import binascii
import pathlib
import plistlib
import urllib.parse


def update_configuration(feed_url: str, public_key: str) -> dict:
    url = urllib.parse.urlsplit(feed_url)
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.fragment:
        raise ValueError("The appcast must be an HTTPS URL without credentials or fragment")
    try:
        key = base64.b64decode(public_key, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ValueError("Expected a base64 Ed25519 public key") from error
    if len(key) != 32:
        raise ValueError("Expected a 32-byte Ed25519 public key")
    return {
        "SUFeedURL": feed_url,
        "SUPublicEDKey": public_key,
        "SUVerifyUpdateBeforeExtraction": True,
        "SURequireSignedFeed": True,
        "SUSignedFeedFailureExpirationInterval": 0,
        "SUAllowsAutomaticUpdates": False,
        "SUAutomaticallyUpdate": False,
        "SUEnableSystemProfiling": False,
        "SUEnableJavaScript": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--info-plist", required=True, type=pathlib.Path)
    parser.add_argument("--feed-url", required=True)
    parser.add_argument("--public-key", required=True, help="Public Ed25519 key, never a private key")
    args = parser.parse_args()
    settings = update_configuration(args.feed_url, args.public_key)
    with args.info_plist.open("rb") as file:
        info = plistlib.load(file)
    info.update(settings)
    # Sparkle asks for permission on second launch; do not silently opt users in.
    info.pop("SUEnableAutomaticChecks", None)
    with args.info_plist.open("wb") as file:
        plistlib.dump(info, file, fmt=plistlib.FMT_XML, sort_keys=False)
    print("Configured signed updates:", args.feed_url)


if __name__ == "__main__":
    main()
