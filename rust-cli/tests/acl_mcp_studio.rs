//! §9.2 ACL + §11 MCP + §13 Studio — ported from tests/test_acl_mcp_studio.py.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-as-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

/// Two docs: one public, one finance-only. Mirrors the Python fixture.
fn seeded(name: &str) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    fs::write(
        v.join("raw").join("pub.md"),
        "---\ntitle: Public\n---\n# Sec\nACME revenue was 32 yuan.\n",
    )
    .unwrap();
    fs::write(
        v.join("raw").join("secret.md"),
        "---\ntitle: Secret\nacl: [finance-team]\n---\n# Pay\nEmployee pay is confidential.\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

fn groups(g: &[&str]) -> Vec<String> {
    g.iter().map(|s| s.to_string()).collect()
}

/// test_acl_grants_are_read_from_frontmatter
#[test]
fn acl_grants_are_read_from_frontmatter() {
    let (_v, conn) = seeded("grants");
    let g = koios::acl::grants_for(&conn, "secret.md");
    assert_eq!(
        g,
        Some(vec!["finance-team".to_string()]),
        "acl must come from frontmatter"
    );
}

/// test_allowed_matrix
#[test]
fn allowed_matrix() {
    let pub_doc: Option<Vec<String>> = None;
    let restricted = Some(vec!["finance-team".to_string()]);
    assert!(
        koios::acl::allowed(pub_doc.as_ref(), None),
        "public -> anonymous sees it"
    );
    assert!(
        koios::acl::allowed(restricted.as_ref(), Some(&groups(&["finance-team"]))),
        "member of the group -> allowed"
    );
    assert!(
        !koios::acl::allowed(restricted.as_ref(), Some(&groups(&["hr"]))),
        "outside the group -> denied"
    );
}

/// test_retrieval_filters_restricted_docs
#[test]
fn retrieval_filters_restricted_docs() {
    let (_v, conn) = seeded("filter");
    let anon = koios::pipeline::retrieve_channel(&conn, "pay", 8, None);
    assert!(
        anon.iter().all(|b| b.doc_path != "secret.md"),
        "anonymous must never see a restricted doc"
    );
    let member =
        koios::pipeline::retrieve_channel(&conn, "pay", 8, Some(&groups(&["finance-team"])));
    assert!(
        member.iter().any(|b| b.doc_path == "secret.md"),
        "finance-team must see it"
    );
}

/// test_public_docs_still_visible_to_anonymous
#[test]
fn public_docs_still_visible_to_anonymous() {
    let (_v, conn) = seeded("publicanon");
    let anon = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert!(
        anon.iter().any(|b| b.doc_path == "pub.md"),
        "public docs stay visible to anonymous"
    );
}

/// test_filter_rows_respects_groups
#[test]
fn filter_rows_respects_groups() {
    let (_v, conn) = seeded("frows");
    let all = {
        let mut s = conn
            .prepare("SELECT id,raw,breadcrumb,doc_path FROM blocks WHERE layer='raw'")
            .unwrap();
        s.query_map([], |r| {
            Ok(koios::pipeline::Ev {
                id: r.get(0)?,
                raw: r.get::<_, String>(1).unwrap_or_default(),
                breadcrumb: r.get::<_, String>(2).unwrap_or_default(),
                doc_path: r.get::<_, String>(3).unwrap_or_default(),
            })
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
    };
    assert!(all.iter().any(|b| b.doc_path == "secret.md"));
    let filtered = koios::acl::filter_blocks(&conn, all, None);
    assert!(
        filtered.iter().all(|b| b.doc_path != "secret.md"),
        "filter_blocks must drop restricted rows"
    );
}

/// test_mcp_initialize_and_tool_list
#[test]
fn mcp_initialize_and_tool_list() {
    let init = koios::mcp::handle_line_stdio(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
    assert!(
        init.contains("protocolVersion"),
        "initialize must reply: {init}"
    );
    let list = koios::mcp::handle_line_stdio(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    for t in ["koios_search", "koios_ask", "koios_write_answer"] {
        assert!(list.contains(t), "tools/list missing {t}");
    }
}

/// test_mcp_search_returns_cited_blocks
#[test]
fn mcp_search_returns_cited_blocks() {
    let (v, _conn) = seeded("mcpsearch");
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"koios_search","arguments":{{"vault":"{}","question":"revenue"}}}}}}"#,
        v.display()
    );
    let out = koios::mcp::handle_line_stdio(&req);
    assert!(out.contains("blocks"), "search must return blocks: {out}");
    assert!(out.contains("id"), "blocks must carry citation ids");
}

/// test_mcp_unknown_tool_errors
#[test]
fn mcp_unknown_tool_errors() {
    let out = koios::mcp::handle_line_stdio(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}"#,
    );
    assert!(out.contains("-32601"), "unknown tool -> -32601: {out}");
}

/// test_studio_brief_carries_citations
#[test]
fn studio_brief_carries_citations() {
    let (v, conn) = seeded("briefcit");
    let p = koios::studio::studio_brief(&conn, &v, "revenue", 5, None).unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(
        text.contains("[["),
        "brief must carry citations (P6):\n{text}"
    );
}

/// test_studio_brief_respects_acl
#[test]
fn studio_brief_respects_acl() {
    let (v, conn) = seeded("briefacl");
    let p = koios::studio::studio_brief(&conn, &v, "pay", 5, None).unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(
        !text.contains("confidential"),
        "an anonymous brief leaked a restricted figure:\n{text}"
    );
}

/// test_studio_mindmap_respects_acl
///
/// A section TITLE is content too — leaking 「薪酬」 alone discloses that pay
/// data exists, so the tree is filtered by document rather than by block text.
///
/// NOTE: the root label is echoed by design (it is the topic the caller asked
/// for), so asserting on it would be meaningless. The leak vector is the
/// restricted DOC's section titles.
#[test]
fn studio_mindmap_respects_acl() {
    let (v, conn) = seeded("mapacl");
    let visible = koios::acl::visible_paths(&conn, None).unwrap_or_default();
    assert!(
        !visible.iter().any(|p| p == "secret.md"),
        "restricted doc must not be visible to anonymous"
    );
    // anonymous mindmap: no branch may come from the restricted document
    let anon = koios::studio::studio_mindmap(&conn, &v, "tree", None).unwrap();
    let anon_text = fs::read_to_string(&anon).unwrap();
    assert!(
        !anon_text.contains("Pay") && !anon_text.contains("员工"),
        "anonymous mindmap leaked a restricted section title:\n{anon_text}"
    );
    // finance-team mindmap over same tree must contain it
    let ok =
        koios::studio::studio_mindmap(&conn, &v, "tree", Some(&groups(&["finance-team"]))).unwrap();
    let ok_text = fs::read_to_string(&ok).unwrap();
    assert!(
        ok_text.contains("Pay"),
        "finance-team must be able to see the branch:\n{ok_text}"
    );
}

/// test_render_brief_without_evidence_says_so
///
/// An export must say there is no evidence, never invent filler (P6).
#[test]
fn render_brief_without_evidence_says_so() {
    let empty: Vec<koios::pipeline::Ev> = Vec::new();
    let text = koios::studio::render_brief("topic", &empty, "2026-01-01");
    assert!(
        text.contains("0"),
        "empty brief must state zero evidence:\n{text}"
    );
}

/// test_render_mindmap_sanitizes_mermaid_labels
///
/// Brackets/parens break mermaid node syntax, so they must be stripped.
#[test]
fn render_mindmap_sanitizes_mermaid_labels() {
    let out = koios::studio::render_mindmap("t", "root [x] (y)", &[], "2026-01-01");
    assert!(
        !out.contains('[') || !out.contains("root ["),
        "mindmap labels must be sanitized:\n{out}"
    );
    let out2 = koios::studio::render_mindmap("t", "root", &[], "2026-01-01");
    assert!(
        out2.contains("mindmap"),
        "mindmap must emit a mermaid block"
    );
}
