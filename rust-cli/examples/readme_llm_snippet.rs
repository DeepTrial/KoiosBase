//! Verifies the README's "connect an LLM" snippet compiles verbatim-ish.
//! The README example is illustrative (call_your_model isn't real); here we
//! substitute a closure that returns its context, which exercises the exact
//! types the documented callable must satisfy.

use koios::connect;
use koios::pipeline::{full_query_with, QueryResult};
use std::path::Path;

fn call_your_model(context: &str) -> String {
    format!("CITED: {context}")
}

fn main() {
    let vault = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let conn = connect(Path::new(&vault)).unwrap();
    let my_llm =
        |_question: &str, context: &str| -> Result<String, String> { Ok(call_your_model(context)) };
    let result: QueryResult = full_query_with(
        &conn,
        "revenue",
        8,
        Some(&["finance-team".into()]),
        Some(&my_llm),
    );
    println!("{}", result.answer);
    println!("{:?}", result.violations);
}
