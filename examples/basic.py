"""Smallest useful imgdl program.

uv run python examples/basic.py
"""

from pathlib import Path

import imgdl

URLS = [
    "https://httpbin.org/image/jpeg",
    "https://httpbin.org/image/png",
    "https://httpbin.org/status/404",  # one deliberate failure
]

out = Path("./downloads")
out.mkdir(exist_ok=True)

results = imgdl.download_images(
    URLS,
    output_dir=str(out),
    config=imgdl.Config(
        max_concurrent=64,
        max_retries=2,
        request_timeout_secs=15.0,
        write_summary=True,  # writes downloads/summary.json
    ),
)

for r in results:
    if r.success:
        print(f"ok    {r.path}  {r.size_bytes} B  {r.elapsed_ms:.0f} ms")
    else:
        print(f"fail  {r.url}  {r.error}")
