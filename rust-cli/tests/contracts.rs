//! check_contracts parity against the Python reference.
//!
//! Every expected value below was produced by actually running
//! koiosbase/generation/contracts.py on these inputs — not written from memory.
//! Two of them are counter-intuitive and are why this test exists:
//!   * an unanswered sentence with no citation still trips the citation rule,
//!     even when the refusal marker is present (case 4);
//!   * a bare assertion with no evidence trips BOTH rules, not just refusal.

use koios::pipeline::{check_contracts, Ev};

fn ev_or_empty(empty: bool) -> Vec<Ev> {
    if empty {
        return vec![];
    }
    vec![Ev {
        id: "report.md#a/b/1".to_string(),
        raw: "x".to_string(),
        breadcrumb: "b".to_string(),
        doc_path: "report.md".to_string(),
    }]
}

/// One golden case: the answer text, whether evidence is empty, and the
/// violations Python reports. Named because the nested tuple is what made this
/// `Vec` too complex for clippy to read at a glance.
type ContractCase<'a> = (&'a str, bool, Vec<(&'a str, &'a str)>);

#[test]
fn contracts_match_python() {
    let cases: Vec<ContractCase> = vec![
        ("营收为32亿元 (report.md#a/b/1)。", false, vec![]),
        (
            "营收为32亿元 (report.md#a/b/1)。现金流为正。",
            false,
            vec![("citation", "1/2 sentences cited")],
        ),
        (
            "凭空回答没有证据。",
            true,
            vec![
                ("citation", "0/1 sentences cited"),
                (
                    "refusal",
                    "answer asserted with no evidence and no refusal marker",
                ),
            ],
        ),
        (
            "资料中未涉及：当前知识库中没有任何相关证据。",
            true,
            vec![("citation", "0/1 sentences cited")],
        ),
    ];

    for (answer, empty, want) in cases {
        let got = check_contracts(answer, &ev_or_empty(empty));
        let mut got_sorted: Vec<(String, String)> = got.into_iter().collect();
        got_sorted.sort();
        let got_refs: Vec<(&str, &str)> = got_sorted
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(got_refs, want, "mismatch on {:?}", answer);
    }
}
