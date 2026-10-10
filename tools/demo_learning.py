"""Exercise the real UI with offline tutoring; save screenshots and a replay video.

Requires playwright and an installed Chrome/Edge browser. Does not use API keys.
Run from the repository root with bridge/src on PYTHONPATH.
"""
import argparse
import json
import tempfile
from pathlib import Path

from eve_bridge.learning.server import mark_count, start
from playwright.sync_api import sync_playwright


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("docs/learning-demo"))
    parser.add_argument("--browser", default="C:/Program Files/Google/Chrome/Application/chrome.exe")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="eve-ui-") as data_dir:
        server = start(data_dir, demo=True, port=0)
        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(executable_path=args.browser, headless=True)
                context = browser.new_context(bypass_csp=True, viewport={"width": 1440, "height": 1000},
                                              record_video_dir=str(args.output / "video"),
                                              record_video_size={"width": 1440, "height": 1000})
                page = context.new_page()
                errors = []
                page.on("pageerror", lambda error: errors.append(str(error)))
                page.goto(server.app.url)
                page.locator("#new-main").click()
                page.locator("#topic").fill("Algebra")
                page.locator("#level").fill("Grades 6–8")
                page.get_by_role("button", name="Let's begin").click()
                page.wait_for_function("document.querySelector('.problem-strip strong')?.textContent === 'Solve 2x + 3 = 11.'")
                page.locator("#ideas").fill("I think I should subtract 3 from both sides.")
                # The whiteboard is tldraw, with the pen already selected. bridge/web-canvas/e2e covers it in depth
                # (reload, lock, old lessons, CSP); here the pen writes the problem out for the recording.
                page.locator("#board .tl-canvas").wait_for()
                board = page.locator("#board .tl-canvas").bounding_box()
                glyphs = {
                    "2": [[(0,8),(8,0),(23,0),(30,8),(30,18),(0,45),(32,45)]],
                    "x": [[(0,12),(26,42)],[(26,12),(0,42)]],
                    "+": [[(0,23),(28,23)],[(14,9),(14,37)]],
                    "3": [[(0,0),(27,0),(12,20),(25,22),(30,32),(25,44),(0,44)]],
                    "=": [[(0,17),(27,17)],[(0,29),(27,29)]],
                    "1": [[(3,10),(14,0),(14,44)],[(2,44),(27,44)]],
                }
                count = 0
                for index, char in enumerate("2x+3=11"):
                    for points in glyphs[char]:
                        count += 1
                        page.mouse.move(board["x"] + 65 + index*48 + points[0][0], board["y"] + 90 + points[0][1])
                        page.mouse.down()
                        for x, y in points[1:]:
                            page.mouse.move(board["x"] + 65 + index*48 + x, board["y"] + 90 + y, steps=3)
                        page.mouse.up()
                page.wait_for_timeout(900)
                page.locator("#hint").click()
                page.wait_for_function("document.querySelector('#feed').textContent.includes('subtract')")
                page.locator("#step").click()
                page.wait_for_function("document.querySelectorAll('.math-step').length === 1")
                assert page.locator(".math-step").inner_text() == "2x = 11 - 3"
                page.screenshot(path=str(args.output / "01-one-step.png"), full_page=True)
                page.wait_for_timeout(2000)
                first_id = page.url.split("#lesson/")[1]
                page.reload()
                page.wait_for_selector("#ideas")
                assert page.locator("#ideas").input_value() == "I think I should subtract 3 from both sides."
                assert mark_count(server.app.store.get(first_id)) == count
                for expected in (2, 3):
                    page.locator("#step").click()
                    page.wait_for_function(f"document.querySelectorAll('.math-step').length === {expected}")
                    page.wait_for_timeout(500)
                page.wait_for_selector("#recap")
                page.locator("#recap").click()
                page.wait_for_function("document.querySelector('#feed').textContent.includes('explain why')")
                page.locator("#history").click()
                page.wait_for_selector("#history-dialog[open]")
                assert page.locator(".history-row").count() >= 7
                page.locator(".history-row summary").first.click()
                page.screenshot(path=str(args.output / "02-saved-work.png"), full_page=True)
                page.locator("#close-history").click()
                # Bring-your-own-problem path, with a real PNG uploaded through the file input.
                page.locator("#new-side").click()
                page.locator('input[value="help"]').check()
                page.locator("#topic").fill("Algebra screenshot")
                page.get_by_role("button", name="Let's begin").click()
                page.wait_for_selector("#upload")
                sample = args.output / "sample-problem.png"
                page.evaluate("""() => { const e=document.createElement('div');e.id='sample';e.textContent='Solve 2x + 3 = 11.';document.body.append(e); }""")
                page.locator("#sample").screenshot(path=str(sample))
                page.evaluate("document.querySelector('#sample').remove()")
                page.locator("#upload").set_input_files(str(sample))
                page.wait_for_selector("#source-preview")
                page.locator("#problem-input").fill("Solve 2x + 3 = 11.")
                page.locator("#recognize").click()
                page.wait_for_selector("#confirm")
                page.locator("#confirm").click()
                page.wait_for_function("!document.querySelector('#confirm')")
                page.locator("#step").click()
                page.wait_for_selector("#error:not(.hidden)")
                assert "ideas first" in page.locator("#error").inner_text()
                page.locator("#ideas").fill("2x = 8")
                page.locator("#step").click()
                page.wait_for_selector("#recap")
                assert page.locator(".math-step").inner_text() == "x = 4"
                page.screenshot(path=str(args.output / "03-screenshot-help.png"), full_page=True)
                page.wait_for_timeout(2000)
                # Early exit and resume preserve the worksheet and completed stage.
                page.locator("#finish").click()
                page.get_by_role("button", name="Resume lesson").wait_for()
                page.reload()
                page.get_by_role("button", name="Resume lesson").click()
                page.wait_for_selector("#recap")
                # A college example checks the upper end of the demo without a model call.
                page.locator("#new-side").click()
                page.locator('input[value="learn"]').check()
                page.locator("#topic").fill("Differential calculus")
                page.locator("#level").fill("College year 1")
                page.get_by_role("button", name="Let's begin").click()
                page.wait_for_function("document.querySelector('.problem-strip strong')?.textContent.includes('derivative')")
                page.locator("#step").click()
                page.wait_for_selector(".math-step")
                assert "3x^(3 - 1)" in page.locator(".math-step").inner_text()
                page.locator("#dashboard-link").click()
                page.wait_for_selector(".cards")
                assert page.locator(".lesson-card").count() == 3
                page.screenshot(path=str(args.output / "04-dashboard.png"), full_page=True)
                page.wait_for_timeout(2000)
                page.set_viewport_size({"width": 390, "height": 844})
                page.screenshot(path=str(args.output / "05-mobile-dashboard.png"), full_page=True)
                assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth")
                assert not errors, errors
                video = page.video
                context.close()
                video.save_as(str(args.output / "eve-learning-demo.webm"))
                browser.close()
                (args.output / "results.json").write_text(json.dumps({"passed": True, "browser_errors": errors,
                    "checks": ["learn topic", "pen drawing", "hint", "exactly one step per click",
                               "reload persistence", "completion and recap", "revision history", "screenshot upload",
                               "problem confirmation", "ideas-first gate", "continue from learner step",
                               "end and resume", "calculus", "dashboard", "mobile layout"]}, indent=2))
        finally:
            server.shutdown()
            server.server_close()
    print("Browser demo passed; screenshots and video saved to", args.output)


if __name__ == "__main__":
    main()
