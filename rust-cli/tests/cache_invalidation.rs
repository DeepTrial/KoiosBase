//! Long-lived MCP graph caches must follow every input to graph shape/weight.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use koios::cache::{corpus_stamp, graph, Adjacency};
use koios::connect;
use rusqlite::Connection;

fn vault(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("koios_cache_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn document(conn: &Connection, path: &str, confidence: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,'')",
        rusqlite::params![path, path, format!("{{\"confidence\":\"{confidence}\"}}")],
    )
    .unwrap();
}

fn block(conn: &Connection, id: &str, doc_path: &str, layer: &str, hash: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO blocks\
         (id,doc_path,section_id,type,breadcrumb,raw,hash,page_range,meta,ordinal,layer) \
         VALUES(?,?,?,'paragraph',?,'body',?,NULL,'{}',1,?)",
        rusqlite::params![id, doc_path, doc_path, id, hash, layer],
    )
    .unwrap();
}

fn link(conn: &Connection, src: &str, dst: &str) {
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES(?,?,'wikilink',1.0)",
        rusqlite::params![src, dst],
    )
    .unwrap();
}

fn fixture(conn: &Connection) {
    document(conn, "entities/e.md", "draft");
    block(conn, "entities/e.md#s/1", "entities/e.md", "derived", "aaaaaaaaaaaaaaaa");
    link(conn, "entities/e.md#s/1", "raw/a.md#s/1");
}

fn fresh_graph(conn: &Connection) -> Adjacency {
    let confidence = koios::retrieval::build_page_confidence(conn);
    koios::retrieval::load_graph_weighted(conn, &confidence).unwrap()
}

#[test]
fn stamp_hashes_content_links_and_confidence_and_is_stable_when_quiet() {
    let v = vault("stamp");
    let conn = connect(&v).unwrap();
    fixture(&conn);
    let original = corpus_stamp(&conn).unwrap();
    assert_eq!(corpus_stamp(&conn).unwrap(), original);

    // Same-length body edits leave both row counts and hash lengths untouched.
    conn.execute(
        "UPDATE blocks SET raw='BODY' WHERE id='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_ne!(corpus_stamp(&conn).unwrap(), original);
    conn.execute(
        "UPDATE blocks SET raw='body' WHERE id='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_eq!(corpus_stamp(&conn).unwrap(), original);

    // A fixed-length hash replacement also defeats SUM(LENGTH(hash)).
    conn.execute(
        "UPDATE blocks SET hash='bbbbbbbbbbbbbbbb' WHERE id='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_ne!(corpus_stamp(&conn).unwrap(), original);
    conn.execute(
        "UPDATE blocks SET hash='aaaaaaaaaaaaaaaa' WHERE id='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_eq!(corpus_stamp(&conn).unwrap(), original);

    conn.execute(
        "UPDATE links SET dst='raw/b.md#s/1' WHERE src='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_ne!(corpus_stamp(&conn).unwrap(), original);
    conn.execute(
        "UPDATE links SET dst='raw/a.md#s/1' WHERE src='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    assert_eq!(corpus_stamp(&conn).unwrap(), original);

    document(&conn, "entities/e.md", "medium");
    assert_ne!(corpus_stamp(&conn).unwrap(), original);
    assert_eq!(corpus_stamp(&conn).unwrap(), corpus_stamp(&conn).unwrap());
    drop(conn);
    let _ = std::fs::remove_dir_all(&v);
}

#[test]
fn graph_reloads_link_weight_delete_restore_and_matches_a_fresh_reader() {
    let v = vault("reload");
    let conn = connect(&v).unwrap();
    fixture(&conn);

    let initial = graph(&v, &conn).unwrap();
    assert_eq!(initial["entities/e.md#s/1"][0].1, 0.0);
    assert_eq!(initial, fresh_graph(&conn));
    assert_eq!(graph(&v, &conn).unwrap(), initial);

    document(&conn, "entities/e.md", "medium");
    let promoted = graph(&v, &conn).unwrap();
    assert_eq!(promoted["entities/e.md#s/1"][0].1, 0.5);
    assert_eq!(promoted, fresh_graph(&conn));

    conn.execute(
        "UPDATE links SET dst='raw/b.md#s/1' WHERE src='entities/e.md#s/1'",
        [],
    )
    .unwrap();
    let moved = graph(&v, &conn).unwrap();
    assert_eq!(moved["entities/e.md#s/1"][0].0, "raw/b.md#s/1");
    assert_eq!(moved, fresh_graph(&conn));

    conn.execute("DELETE FROM links", []).unwrap();
    assert!(graph(&v, &conn).unwrap().is_empty());
    link(&conn, "entities/e.md#s/1", "raw/b.md#s/1");
    assert_eq!(graph(&v, &conn).unwrap(), moved);

    // Simulate a full rebuild with the same row counts but new graph inputs.
    conn.execute("DELETE FROM links", []).unwrap();
    conn.execute("DELETE FROM blocks", []).unwrap();
    block(conn, "entities/e.md#s/1", "entities/e.md", "derived", "cccccccccccccccc");
    link(&conn, "entities/e.md#s/1", "raw/c.md#s/1");
    assert_eq!(graph(&v, &conn).unwrap(), fresh_graph(&conn));
    drop(conn);
    let _ = std::fs::remove_dir_all(&v);
}

fn sorted_targets(adj: &Adjacency, src: &str) -> Vec<String> {
    let mut targets = adj[src]
        .iter()
        .map(|(dst, _)| dst.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets
}

#[test]
fn concurrent_index_switches_whole_snapshots_without_poisoning_the_cache() {
    let v = vault("concurrent");
    let conn = connect(&v).unwrap();
    document(&conn, "raw/e.md", "high");
    block(&conn, "raw/e.md#s/1", "raw/e.md", "raw", "aaaaaaaaaaaaaaaa");
    link(&conn, "raw/e.md#s/1", "raw/a.md#s/1");
    link(&conn, "raw/e.md#s/1", "raw/b.md#s/1");
    assert_eq!(graph(&v, &conn).unwrap(), fresh_graph(&conn));
    drop(conn);

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(5));
    let writer = connect(&v).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let mut readers = Vec::new();
    for _ in 0..4 {
        let path = v.clone();
        let stop = Arc::clone(&stop);
        let barrier = Arc::clone(&barrier);
        readers.push(std::thread::spawn(move || {
            let conn = connect(&path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            barrier.wait();
            while !stop.load(Ordering::Acquire) {
                let adj = graph(&path, &conn).unwrap();
                let targets = sorted_targets(&adj, "raw/e.md#s/1");
                assert!(
                    targets == ["raw/a.md#s/1", "raw/b.md#s/1"]
                        || targets == ["raw/c.md#s/1", "raw/d.md#s/1"],
                    "reader observed a mixed graph generation: {targets:?}"
                );
            }
        }));
    }

    barrier.wait();
    for _ in 0..80 {
        let tx = writer.unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE links SET dst=CASE dst \
             WHEN 'raw/a.md#s/1' THEN 'raw/c.md#s/1' \
             WHEN 'raw/b.md#s/1' THEN 'raw/d.md#s/1' \
             WHEN 'raw/c.md#s/1' THEN 'raw/a.md#s/1' \
             WHEN 'raw/d.md#s/1' THEN 'raw/b.md#s/1' ELSE dst END",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    stop.store(true, Ordering::Release);
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(
        sorted_targets(&graph(&v, &writer).unwrap(), "raw/e.md#s/1"),
        ["raw/a.md#s/1", "raw/b.md#s/1"]
    );
    drop(writer);
    let _ = std::fs::remove_dir_all(&v);
}
