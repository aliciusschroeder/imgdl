"""Minimal CLI: ``imgdl URL [URL ...] -o DIR``.

Exists so the library is testable and demoable without writing a script::

    uvx --from git+https://github.com/aliciusschroeder/imgdl imgdl https://... -o out/
"""

from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence

from imgdl import Config, __version__, download_images


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="imgdl", description=__doc__)
    parser.add_argument("urls", nargs="+", metavar="URL")
    parser.add_argument("-o", "--output-dir", default=".")
    parser.add_argument("-j", "--max-concurrent", type=int, default=200)
    parser.add_argument("--retries", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--summary", action="store_true", help="write summary.json")
    parser.add_argument("--metadata", action="store_true", help="write per-file metadata")
    parser.add_argument("--version", action="version", version=f"imgdl {__version__}")
    parser.add_argument("-q", "--quiet", action="store_true")
    args = parser.parse_args(argv)

    config = Config(
        max_concurrent=args.max_concurrent,
        max_retries=args.retries,
        request_timeout_secs=args.timeout,
        write_summary=args.summary,
        write_metadata=args.metadata,
    )
    results = download_images(args.urls, output_dir=args.output_dir, config=config)

    failed = [r for r in results if not r.success]
    if not args.quiet:
        print(f"{len(results) - len(failed)}/{len(results)} downloaded -> {args.output_dir}")
        for r in failed:
            print(f"  FAIL {r.url}: {r.error}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
