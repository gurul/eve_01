// The shell follows the visual viewport: what is on screen above a phone's keyboard. iOS Safari does not shrink
// the page for its keyboard; it covers the bottom and pans the view to the focused field. --eve-vv-top and
// --eve-vv-height pin the shell to what is visible, so the composer sits on the keyboard and the top bar stays in
// view, and data-keyboard says the keyboard is up (the bottom safe area is then under it, not under the composer).

/** More than this much of the window hidden is a keyboard, not a toolbar. */
const KEYBOARD_PX = 140;

export function trackViewport(root: HTMLElement, win: Window = window): () => void {
  const vv = win.visualViewport;
  let frame = 0;
  const apply = () => {
    frame = 0;
    const height = vv ? vv.height : win.innerHeight;
    const top = vv ? Math.max(0, vv.offsetTop) : 0;
    root.style.setProperty("--eve-vv-top", `${Math.round(top)}px`);
    root.style.setProperty("--eve-vv-height", `${Math.round(height)}px`);
    root.toggleAttribute("data-keyboard", win.innerHeight - height > KEYBOARD_PX);
  };
  const schedule = () => {
    if (!frame) frame = win.requestAnimationFrame(apply);
  };
  apply();
  vv?.addEventListener("resize", schedule);
  vv?.addEventListener("scroll", schedule);
  win.addEventListener("resize", schedule);
  win.addEventListener("orientationchange", schedule);
  return () => {
    if (frame) win.cancelAnimationFrame(frame);
    vv?.removeEventListener("resize", schedule);
    vv?.removeEventListener("scroll", schedule);
    win.removeEventListener("resize", schedule);
    win.removeEventListener("orientationchange", schedule);
  };
}
