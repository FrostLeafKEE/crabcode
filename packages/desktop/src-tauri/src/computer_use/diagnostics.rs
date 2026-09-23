//! Per-request native timings. Nested stage totals may overlap.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Instant;

thread_local! {
    static TIMINGS: RefCell<Option<BTreeMap<&'static str, f64>>> = const { RefCell::new(None) };
}

pub(super) struct Stage {
    name: &'static str,
    started: Instant,
}

impl Stage {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        TIMINGS.with(|timings| {
            if let Some(timings) = timings.borrow_mut().as_mut() {
                *timings.entry(self.name).or_default() +=
                    self.started.elapsed().as_secs_f64() * 1000.0;
            }
        });
    }
}

pub(super) fn measure(run: impl FnOnce() -> Result<Value, String>) -> Result<Value, String> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TIMINGS.with(|timings| *timings.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    TIMINGS.with(|timings| *timings.borrow_mut() = Some(BTreeMap::new()));
    let start = Instant::now();
    let result = run();
    result.map(|mut result| {
        let mut timings = TIMINGS.with(|timings| timings.borrow_mut().take().unwrap_or_default());
        timings.insert("native_total", start.elapsed().as_secs_f64() * 1000.0);
        result["timings_ms"] = json!(timings);
        result
    })
}
