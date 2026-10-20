//! Port of `test_render_tail_memory_does_not_scale_with_history`: Python
//! measured with tracemalloc; this binary counts live heap bytes with its own
//! global allocator. It holds a single test so no other thread allocates
//! while it measures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use eve_audit::{iter_entries, render, Filters, RenderOptions};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Start a measurement: the peak is counted above the bytes live now.
fn start() -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    base
}

fn peak_since(base: usize) -> usize {
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

struct Sink;

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_history(path: &Path, count: usize) {
    let mut f = BufWriter::new(File::create(path).unwrap());
    for i in 0..count {
        let hint = format!("{i}{}", "x".repeat(200));
        writeln!(f, "{}", serde_json::json!({"tool": "Bash", "hint": hint})).unwrap();
    }
}

#[test]
fn test_render_tail_memory_does_not_scale_with_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let small = tmp.path().join("small.jsonl");
    let large = tmp.path().join("large.jsonl");
    write_history(&small, 400);
    write_history(&large, 4000);

    let peak = |path: &Path, last: i64| {
        let opts = RenderOptions { path: Some(path.to_path_buf()), last, ascii_only: true, ..Default::default() };
        let base = start();
        assert_eq!(render(&opts, &mut Sink, false).unwrap(), 0);
        peak_since(base)
    };

    let small_peak = peak(&small, 20);
    let large_peak = peak(&large, 20);
    // Ten times the input must not retain ten times the parsed records.
    assert!(large_peak < 2 * small_peak + 100_000, "{small_peak} {large_peak}");

    // Positive control: the previous approach retains every parsed record.
    let base = start();
    let retained: Vec<_> = iter_entries(Some(&large), Filters::default()).collect::<io::Result<_>>().unwrap();
    let full_peak = peak_since(base);
    assert_eq!(retained.len(), 4000);
    assert!(full_peak > 5 * large_peak, "{large_peak} {full_peak}");
}
