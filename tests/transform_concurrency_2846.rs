//! Exercise the crate's stylesheet scheduling directly, without a host mutex.
//! Keep this as the only test in its binary: the compile counter is process-wide.
use ikigai_gonk::render::{envelope, STYLESHEET};
use std::sync::Barrier;
use std::time::{Duration, Instant};

#[test]
fn concurrent_cold_transforms_share_one_stylesheet_compile() {
    let before = ikigai_xslt::stylesheet_compiles();
    let barrier = Barrier::new(8);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let title = format!("Concurrent page {i}");
                    let document = envelope(
                        "page",
                        &[("view", "home"), ("full", "true"), ("title", &title)],
                        "",
                    );
                    barrier.wait();
                    let html = ikigai_xslt::transform_xml_within(
                        &document,
                        STYLESHEET,
                        false,
                        Duration::from_secs(600),
                    )
                    .expect("a concurrent transform answers");
                    assert!(html.contains(&title), "{html}");
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    eprintln!("eight concurrent cold transforms: {:?}", started.elapsed());
    assert_eq!(
        ikigai_xslt::stylesheet_compiles() - before,
        1,
        "one stylesheet must compile once despite concurrent cold calls"
    );
}
