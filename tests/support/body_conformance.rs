//! Shared wiring and coverage guard for the conformance body checks.

use std::sync::Arc;

use ikigai_conformance::{Check, Report, Suite};
use ikigai_core::{Kernel, MemoryContentStore, Space};

pub fn wire(kernel: Kernel, space: Arc<dyn Space>, suite: Suite) -> (Kernel, Suite) {
    let store = Arc::new(MemoryContentStore::new(1024 * 1024));
    (
        kernel.with_content_store(store.clone()),
        suite.body_space(space).body_input_store(store),
    )
}

pub fn assert_probed(report: &Report) {
    assert!(report.checks.contains(Check::Bodies), "{report}");
    assert!(report.checks.contains(Check::BodyInputs), "{report}");
    // Live answers cannot establish agreement between independent calls. Every other
    // skipped body probe, including missing space/store wiring, must turn the suite red.
    for unprobed in report.unprobed.iter() {
        if matches!(unprobed.check, Check::Bodies | Check::BodyInputs) {
            assert!(
                unprobed.check == Check::Bodies
                    && unprobed
                        .reason
                        .starts_with("uncacheable, time-varying, or volatile answer:"),
                "a deterministic body check was not probed: {unprobed:?}\n{report}"
            );
        }
    }
}
