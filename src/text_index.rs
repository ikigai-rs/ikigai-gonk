//! The store's word index of the ledgers (ledger #4130): what gonk enrolls, and the line the
//! banner prints about it.
//!
//! `ikigai-store`'s `text-index` feature keeps one tantivy index per enrolled named graph,
//! beside the dataset in `<store>/text-index`, and serves it as `urn:iki:store:graph-text`
//! under the same per-graph read grants as `graph-select`. `ikigai-ledger` uses it to rank
//! `items sort=relevance` and to narrow `phrase=` and whole-word searches; its filters stay the
//! definition of a match, so every other listing answers the same with or without it.
//!
//! ★ **What is enrolled is each ledger's own graph, `urn:iki:ledger:graph:{name}`, and only
//! `dcterms:title` (weighted 2x, the ledger's ranking rule) and `ledger:body`.** The store's
//! prefix pattern never enrolls a graph ending in `:deleted` (a ledger's graveyard) or
//! `:session` (its session summaries, which have their own `summaries:{ledger}` grant), so
//! neither is searchable through the index. `tests/text_index_4130.rs` pins both.
//!
//! The index is derived: losing it costs a rebuild, never data. The store commits it without
//! `fsync`, writes a clean-close marker when it shuts down with nothing pending, and rebuilds a
//! graph's index in the background on the next start when the marker is missing (an unclean
//! shutdown), the format or enrollment changed, or the index is gone. Until a rebuild finishes,
//! searches of that ledger scan and `sort=relevance` is refused.

use std::path::{Path, PathBuf};

/// The graph prefix every ledger's own graph shares (`Ledger::graph`).
pub const LEDGER_GRAPHS: &str = "urn:iki:ledger:graph:";

/// `dcterms:title`, an item's title.
pub const TITLE: &str = "http://purl.org/dc/terms/title";

/// The weight of a title hit against a body hit (the ledger's design, decision 6).
pub const TITLE_WEIGHT: f32 = 2.0;

/// `ledger:body`: an item's body and a comment's.
pub fn body() -> String {
    format!("{}body", ikigai_ledger::LEDGER_NS)
}

/// What gonk asks the store to index.
pub fn enrollment() -> ikigai_store::TextEnrollment {
    ikigai_store::TextEnrollment::new()
        .graphs_starting_with(LEDGER_GRAPHS)
        .predicate_weighted(TITLE, TITLE_WEIGHT)
        .predicate(body())
}

/// Where the store keeps the index for a dataset at `store`.
pub fn dir(store: &Path) -> PathBuf {
    store.join("text-index")
}

/// The bytes under `dir`, every file counted once; a missing directory is 0.
pub fn bytes_on_disk(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => bytes_on_disk(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// A size in words a person reads: bytes below 1 KiB, else KiB or MiB with one decimal.
pub fn size_words(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{:.1} MiB", b / (KIB * KIB))
    }
}

/// The banner's line about the index, for a store at `store`.
pub fn banner_line(on: bool, store: &Path) -> String {
    if !on {
        return format!(
            "  search  word index off ({} = false): ledger searches scan, and \"most relevant\" \
             ordering is refused",
            crate::config::TEXT_INDEX_KEY
        );
    }
    let dir = dir(store);
    format!(
        "  search  word index of every ledger's titles, bodies and comments at {} — {} on disk \
         now; derived, so it is rebuilt in the background after an unclean shutdown (searches \
         scan until it is ready)",
        dir.display(),
        size_words(bytes_on_disk(&dir))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_is_each_ledgers_own_graph_never_its_graveyard_or_sessions() {
        let e = enrollment();
        let acme = ikigai_ledger::Ledger::parse("acme").unwrap();
        assert!(e.enrolls(&acme.graph()));
        assert!(e.enrolls(&ikigai_ledger::Ledger::parse("default").unwrap().graph()));
        assert!(!e.enrolls(&acme.deleted_graph()));
        assert!(!e.enrolls(&acme.session_graph()));
        assert!(!e.enrolls("urn:iki:browse:graph"));
        assert!(body().starts_with(ikigai_ledger::LEDGER_NS) && body().ends_with("body"));
    }

    #[test]
    fn sizes_read_as_words() {
        assert_eq!(size_words(0), "0 bytes");
        assert_eq!(size_words(1023), "1023 bytes");
        assert_eq!(size_words(1536), "1.5 KiB");
        assert_eq!(size_words(22 * 1024 * 1024), "22.0 MiB");
    }

    #[test]
    fn the_banner_says_the_size_and_the_rebuild() {
        let dir = std::env::temp_dir().join(format!("gonk-4130-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("text-index/g")).unwrap();
        std::fs::write(dir.join("text-index/g/seg"), vec![0u8; 2048]).unwrap();
        let line = banner_line(true, &dir);
        assert!(line.contains("2.0 KiB on disk"), "{line}");
        assert!(
            line.contains("rebuilt in the background after an unclean shutdown"),
            "{line}"
        );
        let off = banner_line(false, &dir);
        assert!(off.contains("gonk.store.text_index = false"), "{off}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
