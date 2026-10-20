//! The AppKit half of `notes_widget.run`: a borderless, non-activating panel just above the desktop icons.

use std::cell::{OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::{Local, NaiveDate};
use notify::{EventKind, RecursiveMode, Watcher};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType, NSBorderType,
    NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSMenu, NSMenuItem, NSPanel,
    NSScreen, NSScrollView, NSScrollerKnobStyle, NSTextField, NSTextView, NSView, NSWindowCollectionBehavior,
    NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
use objc2_foundation::{
    NSAttributedString, NSDate, NSDictionary, NSMutableAttributedString, NSNotification, NSPoint, NSRect,
    NSRunLoop, NSSize, NSString, NSTimer, NSURL,
};
use tracing::warn;

use crate::{
    change_matters, collect_everything, default_position, journal_dir, load_position, memory_dir, notes_dir,
    position_on_screens, render_sections, render_text, save_position, Note, Rect, EMPTY_TEXT, FONT_SIZE, MAX_NOTES,
    POLL_SECS, STARS_LABEL, TITLE, WIDGET_SIZE,
};

/// watchfiles' `debounce=300` (ms) and its 50 ms step.
const DEBOUNCE: Duration = Duration::from_millis(300);
const STEP: Duration = Duration::from_millis(50);

define_class!(
    /// Read-only text view whose right-click menu is ours, not the editor's.
    #[unsafe(super(NSTextView, objc2_app_kit::NSText, NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EveNotesWidgetTextView"]
    struct NotesTextView;

    impl NotesTextView {
        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            self.window().and_then(|w| w.contentView()).and_then(|v| v.menu())
        }
    }
);

impl NotesTextView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

struct Ivars {
    directory: PathBuf,
    store: PathBuf,
    position_path: PathBuf,
    panel: OnceCell<Retained<NSPanel>>,
    text_view: OnceCell<Retained<NotesTextView>>,
    last_render: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EveNotesWidgetController"]
    #[ivars = Ivars]
    struct Controller;

    impl Controller {
        #[unsafe(method(openLearning:))]
        fn open_learning(&self, _sender: Option<&AnyObject>) {
            // int(os.environ.get("EVE_LEARNING_PORT", "48766")): a value that is not a number opens nothing.
            let raw = std::env::var_os("EVE_LEARNING_PORT")
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_else(|| "48766".to_string());
            let Some(port) = eve_transcripts::py::int(eve_transcripts::py::strip(&raw)) else {
                warn!("notes-widget: invalid literal for int() with base 10: {raw:?}");
                return;
            };
            let url = NSString::from_str(&format!("http://127.0.0.1:{port}/"));
            if let Some(url) = NSURL::URLWithString(&url) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }

        #[unsafe(method(openNotes:))]
        fn open_notes(&self, _sender: Option<&AnyObject>) {
            let directory = &self.ivars().directory;
            if let Err(e) = std::fs::create_dir_all(directory) {
                warn!("notes-widget: cannot create {}: {}", directory.display(), e);
                return;
            }
            open_path(directory);
        }

        #[unsafe(method(openSaid:))]
        fn open_said(&self, _sender: Option<&AnyObject>) {
            let store = &self.ivars().store;
            open_first(&[journal_dir(store), store.clone()]);
        }

        #[unsafe(method(openStars:))]
        fn open_stars(&self, _sender: Option<&AnyObject>) {
            let store = &self.ivars().store;
            open_first(&[store.join("records").join("starred.md"), store.join("records"), store.clone()]);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        /// Called on the main thread only.
        #[unsafe(method(refresh:))]
        fn refresh(&self, _sender: Option<&AnyObject>) {
            self.refresh_now();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, _notification: &NSNotification) {
            let Some(panel) = self.ivars().panel.get() else {
                return;
            };
            let origin = panel.frame().origin;
            if let Err(e) = save_position(origin.x, origin.y, &self.ivars().position_path) {
                warn!("notes-widget: cannot save position: {}", e);
            }
        }
    }
);

// Never create anything under the memory folder from here: the daemon's one-time move skips a target that already
// exists, so a folder made by the widget would strand the owner's records.
fn open_first(paths: &[PathBuf]) {
    if let Some(path) = paths.iter().find(|p| p.exists()) {
        open_path(path);
    }
}

fn open_path(path: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().openURL(&url);
}

fn as_rect(frame: NSRect) -> Rect {
    (frame.origin.x, frame.origin.y, frame.size.width, frame.size.height)
}

fn make_rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn white(white: f64, alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithCalibratedWhite_alpha(white, alpha)
}

impl Controller {
    fn new(mtm: MainThreadMarker, directory: PathBuf, store: PathBuf, position_path: PathBuf) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            directory,
            store,
            position_path,
            panel: OnceCell::new(),
            text_view: OnceCell::new(),
            last_render: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn build(&self) -> anyhow::Result<()> {
        let mtm = self.mtm();
        let (w, h) = WIDGET_SIZE;
        let screens: Vec<Rect> = NSScreen::screens(mtm).iter().map(|s| as_rect(s.visibleFrame())).collect();
        let main = NSScreen::mainScreen(mtm).ok_or_else(|| anyhow!("notes-widget: no main screen"))?;
        let main_rect = as_rect(main.visibleFrame());
        let saved = load_position(&self.ivars().position_path);
        let pos = match saved {
            Some(p) if position_on_screens(Some(p), &screens, WIDGET_SIZE) => p,
            _ => default_position(main_rect, WIDGET_SIZE, crate::SCREEN_MARGIN),
        };

        let rect = make_rect(pos.0, pos.1, w, h);
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setLevel(level() as isize);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setMovableByWindowBackground(true);
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setDelegate(Some(ProtocolObject::from_ref(self)));

        // Background: dark, rounded, 75 % alpha. A plain layer beats NSVisualEffectView here — no vibrancy
        // dependence on what sits behind a desktop-level window.
        let content = NSView::initWithFrame(NSView::alloc(mtm), make_rect(0.0, 0.0, w, h));
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            layer.setBackgroundColor(Some(&white(0.08, 0.75).CGColor()));
            layer.setCornerRadius(14.0);
            layer.setMasksToBounds(true);
        }
        unsafe { content.setMenu(Some(&self.context_menu())) };
        panel.setContentView(Some(&content));

        let pad = 16.0;
        let title = NSTextField::labelWithString(&NSString::from_str(TITLE), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        title.setTextColor(Some(&white(1.0, 0.92)));
        title.setFrame(make_rect(pad, h - pad - 18.0, w - 2.0 * pad, 18.0));
        title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewWidthSizable);
        content.addSubview(&title);

        let scroll =
            NSScrollView::initWithFrame(NSScrollView::alloc(mtm), make_rect(pad, pad, w - 2.0 * pad, h - 3.0 * pad - 18.0));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setBorderType(NSBorderType::NoBorder);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll.setScrollerKnobStyle(NSScrollerKnobStyle::Light);

        let tv = NotesTextView::new(mtm, scroll.contentView().bounds());
        tv.setEditable(false);
        tv.setSelectable(true);
        tv.setDrawsBackground(false);
        tv.setRichText(false);
        tv.setFont(Some(&NSFont::systemFontOfSize(FONT_SIZE)));
        tv.setTextColor(Some(&white(0.95, 1.0)));
        tv.setVerticallyResizable(true);
        tv.setHorizontallyResizable(false);
        tv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(container) = unsafe { tv.textContainer() } {
            container.setWidthTracksTextView(true);
        }
        tv.setTextContainerInset(NSSize::new(0.0, 2.0));
        scroll.setDocumentView(Some(&tv));
        content.addSubview(&scroll);

        let _ = self.ivars().panel.set(panel.clone());
        let _ = self.ivars().text_view.set(tv);
        self.refresh_now();
        panel.orderFrontRegardless();
        Ok(())
    }

    fn context_menu(&self) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(TITLE));
        let empty = NSString::from_str("");
        let items = [
            ("Open learning dashboard", sel!(openLearning:)),
            ("Open what it saw", sel!(openNotes:)),
            ("Open what was said", sel!(openSaid:)),
            ("Remembered for good\u{2026}", sel!(openStars:)),
        ];
        for (title, action) in items {
            let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), Some(action), &empty) };
            unsafe { item.setTarget(Some(self)) };
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let quit = unsafe { menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str("Quit"), Some(sel!(quit:)), &empty) };
        unsafe { quit.setTarget(Some(self)) };
        menu
    }

    fn refresh_now(&self) {
        let today = Local::now().date_naive();
        let iv = self.ivars();
        let (notes, starred) = collect_everything(&iv.directory, &iv.store, Some(today), MAX_NOTES);
        let plain = render_text(&notes, Some(today), &starred);
        if iv.last_render.borrow().as_deref() == Some(plain.as_str()) {
            return;
        }
        let rendered = attributed(&notes, today, &starred);
        *iv.last_render.borrow_mut() = Some(plain);
        if let Some(tv) = iv.text_view.get() {
            if let Some(storage) = unsafe { tv.textStorage() } {
                storage.setAttributedString(&rendered);
            }
            tv.scrollPoint(NSPoint::new(0.0, 0.0));
        }
    }
}

/// The panel's rendering: the same text as `render_text`, with bold dim headers and light body lines.
fn attributed(notes: &[Note], today: NaiveDate, starred: &[String]) -> Retained<NSMutableAttributedString> {
    let body = NSFont::systemFontOfSize(FONT_SIZE);
    let head = NSFont::boldSystemFontOfSize(FONT_SIZE - 1.0);
    let light = white(0.95, 1.0);
    let dim = white(0.75, 1.0);
    let out = NSMutableAttributedString::new();

    let add = |text: &str, font: &NSFont, colour: &NSColor| {
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [font.as_ref(), colour.as_ref()];
        let attrs = NSDictionary::from_slices(&keys, &values);
        let piece = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(text),
                Some(&attrs),
            )
        };
        out.appendAttributedString(&piece);
    };

    if notes.is_empty() && starred.is_empty() {
        add(EMPTY_TEXT, &body, &dim);
        return out;
    }
    let mut first = true;
    if !starred.is_empty() {
        add(&format!("{STARS_LABEL}\n"), &head, &dim);
        let lines: Vec<String> = starred.iter().rev().map(|c| format!("\u{2605}  {c}")).collect();
        add(&lines.join("\n"), &body, &light);
        first = false;
    }
    for (label, group) in render_sections(notes, Some(today)) {
        add(&format!("{}{label}\n", if first { "" } else { "\n\n" }), &head, &dim);
        first = false;
        let lines: Vec<String> = group.iter().map(|n| format!("{}  {}", n.time, n.text)).collect();
        add(&lines.join("\n"), &body, &light);
    }
    out
}

/// `Quartz.kCGDesktopIconWindowLevel + 1`: just above the desktop icons.
fn level() -> i32 {
    CGWindowLevelForKey(CGWindowLevelKey::DesktopIconWindowLevelKey) + 1
}

/// The controller, handed to the watcher thread only to post `refresh:` to the main thread.
struct MainThreadTarget(Retained<AnyObject>);

// SAFETY: the only message sent from another thread is performSelectorOnMainThread:withObject:waitUntilDone:, which
// NSObject documents as callable from any thread; retain/release are thread-safe.
unsafe impl Send for MainThreadTarget {}

impl MainThreadTarget {
    fn post_refresh(&self) {
        let none: Option<&AnyObject> = None;
        unsafe {
            let _: () = msg_send![
                &*self.0,
                performSelectorOnMainThread: sel!(refresh:),
                withObject: none,
                waitUntilDone: false
            ];
        }
    }
}

/// The watcher thread: every debounced batch of changes that matters posts a refresh. A dead watcher degrades to
/// polling.
fn watch(target: MainThreadTarget, directory: PathBuf, store: PathBuf, stop: Arc<AtomicBool>) {
    let result = (|| -> notify::Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        let records_dir = store.join("records");
        watcher.watch(&directory, RecursiveMode::Recursive)?;
        if records_dir.exists() {
            watcher.watch(&records_dir, RecursiveMode::Recursive)?;
        }
        let relevant = |ev: notify::Result<notify::Event>| -> notify::Result<bool> {
            let ev = ev?;
            Ok(!matches!(ev.kind, EventKind::Access(_)) && ev.paths.iter().any(|p| change_matters(p)))
        };
        while !stop.load(Ordering::SeqCst) {
            match rx.recv_timeout(STEP) {
                Ok(ev) => {
                    if !relevant(ev)? {
                        continue;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(notify::Error::generic("the watcher closed"));
                }
            }
            // A batch: keep gathering until a quiet step or the debounce window is up.
            let started = Instant::now();
            while started.elapsed() < DEBOUNCE && !stop.load(Ordering::SeqCst) {
                match rx.recv_timeout(STEP) {
                    Ok(ev) => {
                        relevant(ev)?;
                    }
                    Err(_) => break,
                }
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            target.post_refresh();
        }
        Ok(())
    })();
    if let Err(e) = result {
        warn!("notes-widget: file watcher stopped ({}); polling every {:.0}s", e, POLL_SECS);
    }
}

pub(crate) fn run(
    once: bool,
    directory: Option<PathBuf>,
    position_path: PathBuf,
    store: Option<PathBuf>,
) -> anyhow::Result<i32> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("notes-widget: AppKit must run on the main thread"));
    };
    let directory = directory.unwrap_or_else(notes_dir);
    let store = store.unwrap_or_else(memory_dir);
    std::fs::create_dir_all(&directory).with_context(|| format!("cannot create {}", directory.display()))?;

    let app = NSApplication::sharedApplication(mtm);
    // Prohibited = LSBackgroundOnly: no Dock tile, no menu bar, never activates. A non-activating panel still
    // receives mouse events, which is all dragging and the context menu need.
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);

    let controller = Controller::new(mtm, directory.clone(), store.clone(), position_path.clone());
    controller.build()?;

    // Backstop poll in case the file watcher misses a write.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            POLL_SECS,
            &controller,
            sel!(refresh:),
            None,
            true,
        )
    };

    let stop = Arc::new(AtomicBool::new(false));
    let target = MainThreadTarget(Retained::into_super(Retained::into_super(controller.clone())));
    {
        let (directory, store, stop) = (directory.clone(), store.clone(), stop.clone());
        thread::Builder::new()
            .name("notes-watch".into())
            .spawn(move || watch(target, directory, store, stop))
            .context("cannot start the file watcher")?;
    }

    let panel = controller.ivars().panel.get().cloned().ok_or_else(|| anyhow!("notes-widget: no panel"))?;
    let frame = panel.frame();
    println!(
        "notes-widget: dir={} level={} frame=({:.0},{:.0} {:.0}x{:.0}) position_file={}",
        directory.display(),
        level(),
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        position_path.display()
    );

    if once {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(1.0));
        println!(
            "notes-widget: window visible={} number={}",
            if panel.isVisible() { "True" } else { "False" },
            panel.windowNumber()
        );
        println!("--- rendered ---");
        println!("{}", controller.ivars().last_render.borrow().as_deref().unwrap_or("None"));
        stop.store(true, Ordering::SeqCst);
        return Ok(0);
    }

    app.run();
    stop.store(true, Ordering::SeqCst);
    Ok(0)
}
