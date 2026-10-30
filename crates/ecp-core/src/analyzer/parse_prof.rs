//! Per-pass timing inside `parse_file`, read by the `ECP_PROF` printer in
//! `pipeline.rs`. Disabled cost is one relaxed atomic load per span.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Clone, Copy)]
pub enum Pass {
    Tree,
    Query,
    Build,
    Walk,
    Imports,
    Calls,
}

impl Pass {
    /// The last variant plus one; `ALL` lists every variant in discriminant
    /// order, which a test checks.
    pub const COUNT: usize = Pass::Calls as usize + 1;

    pub const ALL: [Pass; Pass::COUNT] = [
        Pass::Tree,
        Pass::Query,
        Pass::Build,
        Pass::Walk,
        Pass::Imports,
        Pass::Calls,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Pass::Tree => "tree",
            Pass::Query => "query",
            Pass::Build => "build",
            Pass::Walk => "walk",
            Pass::Imports => "imports",
            Pass::Calls => "calls",
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static TOTALS: Cell<[u64; Pass::COUNT]> = const { Cell::new([0; Pass::COUNT]) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub struct Span {
    start: Option<Instant>,
    pass: Pass,
    // `!Send`: `Drop` writes this thread's `TOTALS` and `ACTIVE`, so a span
    // dropped on another thread would book its time there and leave the
    // origin thread's `ACTIVE` set.
    _thread: PhantomData<*const ()>,
}

#[must_use = "a span measures until it is dropped"]
pub fn span(pass: Pass) -> Span {
    let start = if enabled() {
        debug_assert!(
            !ACTIVE.with(Cell::get),
            "parse_prof spans must be disjoint on one thread"
        );
        ACTIVE.with(|a| a.set(true));
        Some(Instant::now())
    } else {
        None
    };
    Span {
        start,
        pass,
        _thread: PhantomData,
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let ns = start.elapsed().as_nanos() as u64;
        TOTALS.with(|t| {
            let mut totals = t.get();
            totals[self.pass as usize] += ns;
            t.set(totals);
        });
        ACTIVE.with(|a| a.set(false));
    }
}

pub fn reset() {
    TOTALS.with(|t| t.set([0; Pass::COUNT]));
}

pub fn take() -> [u64; Pass::COUNT] {
    TOTALS.with(|t| t.replace([0; Pass::COUNT]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn busy() {
        let t = Instant::now();
        while t.elapsed().as_nanos() < 1000 {}
    }

    #[test]
    fn test_pass_all_lists_every_variant_in_order() {
        for (i, pass) in Pass::ALL.into_iter().enumerate() {
            assert_eq!(pass as usize, i, "Pass::ALL[{i}] is {}", pass.name());
        }
    }

    #[test]
    fn test_parse_prof_disabled_records_nothing() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        reset();
        {
            let _p = span(Pass::Tree);
            busy();
        }
        assert_eq!(take(), [0; Pass::COUNT]);
    }

    #[test]
    fn test_parse_prof_enabled_records_and_take_resets() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        reset();
        {
            let _p = span(Pass::Query);
            busy();
        }
        let got = take();
        set_enabled(false);
        assert!(got[Pass::Query as usize] > 0);
        assert_eq!(got[Pass::Tree as usize], 0);
    }

    #[test]
    fn test_parse_prof_second_take_returns_zeros() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        reset();
        {
            let _p = span(Pass::Walk);
            busy();
        }
        let first = take();
        let second = take();
        set_enabled(false);
        assert!(first[Pass::Walk as usize] > 0);
        assert_eq!(second, [0; Pass::COUNT]);
    }

    fn returns_early_inside_span() -> Option<u8> {
        let _p = span(Pass::Tree);
        // An empty span can read 0 ns on a coarse or fast clock (macOS CI).
        busy();
        None?;
        Some(1)
    }

    #[test]
    fn test_parse_prof_early_return_closes_span() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        reset();
        assert!(returns_early_inside_span().is_none());
        {
            let _p = span(Pass::Query);
            busy();
        }
        let got = take();
        set_enabled(false);
        assert!(got[Pass::Tree as usize] > 0);
        assert!(got[Pass::Query as usize] > 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "disjoint")]
    fn test_parse_prof_nested_span_panics_in_debug() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        let _outer = span(Pass::Tree);
        let _inner = span(Pass::Query);
    }
}
