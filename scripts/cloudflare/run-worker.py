"""Run Wrangler deployment with email-redacted console output."""
import argparse
import sys

from common import run_wrangler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true", help="Validate without publishing")
    mode.add_argument("--publish", action="store_true", help="Publish the Worker and built web assets")
    arguments = parser.parse_args()
    return run_wrangler(["deploy"] + (["--dry-run"] if arguments.dry_run else []), timeout=120)


if __name__ == "__main__":
    sys.exit(main())
