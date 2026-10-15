#!/usr/bin/env python3
"""Sign a release directory using an existing Keychain key; verify with its public key.

Run after notarizing and stapling final DMGs. Upload the resulting appcast.xml
and exact DMGs to the same GitHub Release. Never modify them after signing.
This script does not create/export keys, upload assets, or use CI secrets.
"""
import argparse
import base64
import pathlib
import re
import subprocess
import urllib.parse
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parent.parent
SPARKLE_NAMESPACE = "http://www.andymatuschak.org/xml-namespaces/sparkle"


def run(*arguments):
    subprocess.run([str(argument) for argument in arguments], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archives", type=pathlib.Path, required=True)
    parser.add_argument("--download-url-prefix", required=True, help="https://github.com/ORG/REPO/releases/download/vVERSION/")
    parser.add_argument("--keychain-account", required=True)
    parser.add_argument("--public-key", required=True)
    parser.add_argument("--tools", type=pathlib.Path, default=ROOT / "apps/macos/.build/artifacts/sparkle/Sparkle/bin")
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.download_url_prefix)
    if url.scheme != "https" or url.hostname != "github.com" or url.username or url.password or url.query or url.fragment or not re.fullmatch(r"/[^/]+/[^/]+/releases/download/[^/]+/", url.path):
        raise SystemExit("Expected an immutable HTTPS GitHub release asset prefix ending in /")
    if len(base64.b64decode(args.public_key, validate=True)) != 32:
        raise SystemExit("Invalid public Ed25519 key")
    archives = args.archives.resolve(strict=True)
    dmgs = sorted(archives.glob("*.dmg"))
    if not dmgs:
        raise SystemExit("The release directory must contain a notarized .dmg")
    for dmg in dmgs:
        run("codesign", "--verify", "--strict", dmg)
        run("xcrun", "stapler", "validate", dmg)
    # -p only reads an existing PUBLIC key; it never creates or exports a secret.
    public_key = subprocess.check_output([str(args.tools / "generate_keys"), "--account", args.keychain_account, "-p"], text=True).strip()
    if public_key != args.public_key:
        raise SystemExit("The Keychain signing account does not match the pinned public key")
    feed = archives / "appcast.xml"
    run(args.tools / "generate_appcast", "--account", args.keychain_account,
        "--download-url-prefix", args.download_url_prefix, "--maximum-deltas", "0",
        "--maximum-versions", "0", "-o", feed, archives)
    run("swift", ROOT / "scripts/verify-update-signature.swift", args.public_key, feed)
    tree = ET.parse(feed)
    enclosures = tree.findall("./channel/item/enclosure")
    if not enclosures:
        raise SystemExit("No update archives in the generated feed")
    for enclosure in enclosures:
        asset_url = enclosure.attrib.get("url", "")
        if not asset_url.startswith(args.download_url_prefix):
            raise SystemExit("Unexpected enclosure outside this release: " + asset_url)
        name = urllib.parse.unquote(asset_url[len(args.download_url_prefix):])
        if not name or pathlib.PurePosixPath(name).name != name:
            raise SystemExit("Invalid archive name in feed")
        archive = archives / name
        if archive.is_symlink() or not archive.is_file() or archive.stat().st_size != int(enclosure.attrib.get("length", "-1")):
            raise SystemExit("Archive missing or length mismatch: " + name)
        signature = enclosure.attrib.get("{" + SPARKLE_NAMESPACE + "}edSignature", "")
        run("swift", ROOT / "scripts/verify-update-signature.swift", args.public_key, archive, signature)
    print("Signed feed and archives verified:", feed)


if __name__ == "__main__":
    main()
