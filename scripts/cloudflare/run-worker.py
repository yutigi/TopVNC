"""Dry-run Wrangler or publish through the guarded deployment workflow."""
import argparse
import sys

from common import run_deployment, run_wrangler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true", help="Validate without publishing")
    mode.add_argument("--publish", action="store_true", help="Build, test, and publish from clean, current main")
    arguments = parser.parse_args()
    if arguments.publish:
        return run_deployment(timeout=1800)
    return run_wrangler(["deploy", "--dry-run"], timeout=120)


if __name__ == "__main__":
    sys.exit(main())
