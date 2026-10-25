#!/usr/bin/env python3
"""Headless check of the Tranche 3 exit criterion: Open MCT shows history after a page reload.

Opens the Chronus Open MCT page (``npm run dev`` in this folder, gateway running) on one telemetry
point in a Telemetry Table, then:

  1. counts the rows loaded from the gateway's history API,
  2. optionally runs ``--send`` (e.g. a ``chronus-replay`` command) and checks that new rows arrive
     in realtime over the WebSocket,
  3. reloads the page and checks every row is still there (served from history again).

Requires Python Playwright with a Chromium build (``pip install playwright && playwright install
chromium``). Exit status is non-zero if any step fails; ``--screenshot`` saves the final page.

Example (from the repository root, gateway + ``npm run dev`` already running):

    python3 demo/openmct/verify_history.py --expect-at-least 1 \\
        --send "cargo run -q -p chronus-replay -- -f demo/replay/fixtures/tm_frames_lab.hex --delay-ms 20"
"""

import argparse
import shlex
import subprocess
import sys

from playwright.sync_api import sync_playwright

# The table footer reads "LATEST 50 ROWS" (or "50 ROWS" once unlimited); the DOM itself only holds
# the rows in view, so the footer is the reliable count.
ROWS_JS = """() => {
  const el = document.querySelector('.c-table-indicator__row-count');
  const m = el && el.textContent.match(/(\\d+)/);
  return m ? Number(m[1]) : 0;
}"""


def show_all_rows(page):
    """Open MCT tables cap at the latest 50 rows by default; lift the cap."""
    button = page.get_by_text("Show unlimited", exact=False)
    if button.count() > 0:
        button.first.click()


def wait_rows(page, at_least, timeout_s):
    """Polls the table until it has at least `at_least` rows; returns the final count."""
    count = 0
    for _ in range(int(timeout_s * 4)):
        show_all_rows(page)
        count = page.evaluate(ROWS_JS)
        if count >= at_least:
            return settle(page, count)
        page.wait_for_timeout(250)
    return count


def settle(page, count, quiet_ms=2000):
    """Waits until the row count has not changed for `quiet_ms` (realtime rows arrive in batches)."""
    still = 0
    while still < quiet_ms:
        page.wait_for_timeout(250)
        now = page.evaluate(ROWS_JS)
        still = still + 250 if now == count else 0
        count = now
    return count


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://127.0.0.1:5174", help="Open MCT page (vite dev server)")
    ap.add_argument("--point", default="chronus.physics.elevation_deg", help="dictionary key to open")
    ap.add_argument("--window-min", type=int, default=15, help="realtime window, minutes")
    ap.add_argument("--expect-at-least", type=int, default=1, help="rows expected from history on load")
    ap.add_argument("--send", help="command that sends more telemetry while the page is open")
    ap.add_argument("--timeout", type=float, default=20.0, help="seconds to wait at each step")
    ap.add_argument("--screenshot", help="save a PNG of the page after the reload")
    args = ap.parse_args()

    delta = args.window_min * 60 * 1000
    url = (
        f"{args.url}/#/browse/chronus.taxonomy:spacecraft/chronus.taxonomy:{args.point}"
        f"?tc.mode=local&tc.startDelta={delta}&tc.endDelta=15000&tc.timeSystem=utc&view=table"
    )
    ok = True
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 900})
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))

        page.goto(url)
        loaded = wait_rows(page, args.expect_at_least, args.timeout)
        print(f"history on load:     {loaded} row(s)")
        ok &= loaded >= args.expect_at_least

        live = loaded
        if args.send:
            subprocess.run(shlex.split(args.send), check=True)
            live = wait_rows(page, loaded + 1, args.timeout)
            print(f"after realtime send: {live} row(s)")
            ok &= live > loaded

        page.reload()
        reloaded = wait_rows(page, live, args.timeout)
        print(f"after page reload:   {reloaded} row(s)")
        ok &= reloaded >= live

        if args.screenshot:
            page.screenshot(path=args.screenshot)
        if errors:
            print("page errors:", *errors, sep="\n  ")
            ok = False
        browser.close()

    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
