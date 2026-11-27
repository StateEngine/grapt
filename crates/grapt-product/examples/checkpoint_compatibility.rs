#![cfg(feature = "checkpoint-fixture")]

use grapt_product::checkpoint_fixture::run_fixture_corpus;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("a new disposable root is required")?);
    if args.next().is_some() {
        return Err("only one disposable root may be supplied".into());
    }
    if root.exists() && std::fs::read_dir(&root)?.next().is_some() {
        return Err("the disposable root already contains data".into());
    }
    std::fs::create_dir_all(&root)?;
    let actions = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/checkpoint-compatibility/actions.jsonl");
    let receipts = run_fixture_corpus(&root, &actions)?;
    println!("{}", serde_json::to_string(&receipts)?);
    Ok(())
}
