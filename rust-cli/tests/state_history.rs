//! §8.2 history: hidden states are reachable on request, and only then.
//!
//! §8.2 disposes of superseded and retracted blocks, which is right -- but it
//! was unconditional, so a superseded card was simply absent with no way to
//! ask for it. These pin both halves: hidden by default, labelled when asked.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-hist-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seeded(name: &str, state: Option<&str>) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    let fm = match state {
        Some(s) => format!("---\ntitle: 卡片\nstate: {s}\n---\n\n"),
        None => "---\ntitle: 卡片\n---\n\n".to_string(),
    };
    fs::write(
        v.join("raw").join("c.md"),
        format!("{fm}# 卡片\n\n## 节\nzebra42 独有内容甲乙丙。\n"),
    )
    .unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

#[test]
fn a_superseded_card_is_absent_by_default() {
    let (v, conn) = seeded("default", Some("superseded"));
    let out =
        koios::pipeline::retrieve_all(&v, &conn, "zebra42", 10, None, &Default::default()).unwrap();
    assert!(
        !out.iter().any(|b| b.doc_path == "c.md"),
        "a superseded card must not appear without being asked for"
    );
}

#[test]
fn a_superseded_card_returns_when_requested_and_is_labelled() {
    let (v, conn) = seeded("asked", Some("superseded"));
    let baseline =
        koios::pipeline::retrieve_all(&v, &conn, "zebra42", 10, None, &Default::default()).unwrap();
    eprintln!(
        "DBG baseline ids = {:?}",
        baseline.iter().map(|b| b.id.clone()).collect::<Vec<_>>()
    );
    let states = vec!["superseded".to_string()];
    let out = koios::pipeline::retrieve_all(
        &v,
        &conn,
        "zebra42",
        10,
        None,
        &koios::pipeline::Query {
            include_states: &states,
            ..Default::default()
        },
    )
    .unwrap();
    let conn2 = connect(&v).unwrap();
    let mut q = conn2
        .prepare("SELECT block_id,state FROM block_state")
        .unwrap();
    let rows: Vec<(String, String)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .flatten()
        .collect();
    eprintln!("DBG block_state = {:?}", rows);
    eprintln!(
        "DBG out ids = {:?}",
        out.iter().map(|b| b.id.clone()).collect::<Vec<_>>()
    );
    let hit = out.iter().find(|b| b.doc_path == "c.md");
    assert!(hit.is_some(), "explicit request must return the card");
    assert_eq!(
        hit.unwrap().state.as_deref(),
        Some("superseded"),
        "a historical block must say it is not current"
    );
}

#[test]
fn removing_the_state_restores_the_card() {
    let (v, conn) = seeded("restore", Some("superseded"));
    // Edit the card back to active and re-index.
    fs::write(
        v.join("raw").join("c.md"),
        "---\ntitle: 卡片\nstate: active\n---\n\n# 卡片\n\n## 节\nzebra42 独有内容甲乙丙。\n",
    )
    .unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();
    let out =
        koios::pipeline::retrieve_all(&v, &conn, "zebra42", 10, None, &Default::default()).unwrap();
    assert!(
        out.iter().any(|b| b.doc_path == "c.md"),
        "state is derived from Markdown, so removing it must restore the card"
    );
    koios::embed::set_allow_lexical(false);
}

#[test]
fn retracted_stays_hidden_unless_named() {
    let (v, conn) = seeded("retracted", Some("retracted"));
    let out =
        koios::pipeline::retrieve_all(&v, &conn, "zebra42", 10, None, &Default::default()).unwrap();
    assert!(
        !out.iter().any(|b| b.doc_path == "c.md"),
        "retracted must be hidden by default"
    );
    let states = vec!["retracted".to_string()];
    let out = koios::pipeline::retrieve_all(
        &v,
        &conn,
        "zebra42",
        10,
        None,
        &koios::pipeline::Query {
            include_states: &states,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        out.iter().any(|b| b.doc_path == "c.md"),
        "retracted must still be retrievable when explicitly requested"
    );
}
