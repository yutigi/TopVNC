"""List Cloudflare identity, VPC services, and tunnels (read only)."""
import argparse
import sys

from common import run_wrangler


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    failed = False
    for arguments in (["whoami"], ["vpc", "service", "list"], ["tunnel", "list"]):
        print("CHECK:", " ".join(arguments), flush=True)
        failed |= run_wrangler(arguments, timeout=35) != 0
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
