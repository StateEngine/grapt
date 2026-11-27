use super::*;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn tool(name: &str, input: serde_json::Value) -> Result<DurableTool> {
    Ok(DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_diff_0001")?,
        name: name.into(),
        state: DurableToolState::Completed {
            input: input.to_string(),
            output: r#"{"created":true}"#.into(),
        },
        progress: Vec::new(),
    })
}

#[test]
fn edit_projection_preserves_context_marks_and_receipt_coordinates() -> Result {
    let mut tool = tool(
        "edit",
        serde_json::json!({"path":"a.rs", "old_text":"old", "new_text":"new"}),
    )?;
    if let DurableToolState::Completed { output, .. } = &mut tool.state {
        *output = serde_json::json!({"files":[{"patchOmitted":false,
            "patch":"--- a.rs\n+++ a.rs\n@@ -19,3 +19,3 @@\n-fn f() {\n-  old();\n-}\n+fn f() {\n+  new();\n+}\n"}]}).to_string();
    }
    let (text, marks) = present(&tool, "tool edit").ok_or("diff missing")?;
    assert_eq!(text.lines().count(), marks.len());
    assert!(marks.contains(&Mark::Context));
    assert_eq!(marks.iter().filter(|m| **m == Mark::Insert).count(), 1);
    assert_eq!(marks.iter().filter(|m| **m == Mark::Delete).count(), 1);
    assert!(text
        .lines()
        .any(|l| l.split_whitespace().take(2).eq(["19", "19"])));
    // A corrupt receipt cannot lend coordinates to the requested snippets.
    if let DurableToolState::Completed { output, .. } = &mut tool.state {
        *output = "{}".into();
    }
    let (text, _) = present(&tool, "tool edit").ok_or("fallback missing")?;
    assert!(text.contains('?'));
    Ok(())
}

#[test]
fn patch_inputs_keep_files_hunks_failures_and_controls_distinct() -> Result {
    let input = serde_json::json!({"input":"*** Begin Patch\n*** Update File: a.rs\n@@ first\n old\n-before\n+after\n@@ second\n-again\n+changed\n*** Add File: b.txt\n+hello\n*** End Patch"});
    let mut patch_tool = tool("apply_patch", input.clone())?;
    let (text, marks) = present(&patch_tool, "tool apply_patch").ok_or("patch missing")?;
    assert_eq!(marks.iter().filter(|m| **m == Mark::Header).count(), 2);
    assert!(marks.contains(&Mark::Gap) && text.contains("a.rs") && text.contains("b.txt"));
    assert!(text.contains("applied diff unavailable") && text.contains("removed ? lines"));
    // Completed receipt metadata owns offsets and counts, including overwrites
    // and deletions; the requested Add File text cannot prove a zero preimage.
    if let DurableToolState::Completed { output, .. } = &mut patch_tool.state {
        *output = serde_json::json!({"files":[
            {"relativePath":"a.rs","countsAvailable":true,"additions":1,"deletions":1,
             "diff":{"firstLine":41,"oldText":"before\n","newText":"after\n","omittedOldLines":0,"omittedNewLines":0}},
            {"relativePath":"b.txt","countsAvailable":true,"additions":1,"deletions":3,
             "diff":{"firstLine":1,"oldText":"old1\nold2\nold3\n","newText":"hello\n","omittedOldLines":0,"omittedNewLines":0}},
            {"relativePath":"deleted.txt","countsAvailable":true,"additions":0,"deletions":2,
             "diff":{"firstLine":1,"oldText":"one\ntwo\n","newText":"","omittedOldLines":0,"omittedNewLines":0}}
        ]}).to_string();
    }
    let (text, marks) = present(&patch_tool, "tool apply_patch").ok_or("receipt absent")?;
    assert!(
        text.contains("41") && text.contains("removed 3 lines") && text.contains("removed 2 lines")
    );
    assert_eq!(marks.iter().filter(|m| **m == Mark::Header).count(), 3);
    if let DurableToolState::Completed { output, .. } = &mut patch_tool.state {
        *output = serde_json::json!({"files":[{"relativePath":"legacy.bin", "patchOmitted":true,
            "countsAvailable":false,"additions":null,"deletions":null,"diff":null}]})
        .to_string();
    }
    let (text, _) = present(&patch_tool, "tool apply_patch").ok_or("omission absent")?;
    assert!(text.contains("diff unavailable") && text.contains("removed ? lines"));

    patch_tool.state = DurableToolState::Error {
        input: input.to_string(),
        output: None,
        error: crate::wire::DurableError {
            kind: "rejected".into(),
            message: "Permission denied".into(),
        },
    };
    let (text, marks) = present(&patch_tool, "tool apply_patch").ok_or("attempt missing")?;
    assert!(text.contains("not applied") && text.contains("Permission denied"));
    assert!(marks.contains(&Mark::Insert));
    let escaped = tool(
        "write",
        serde_json::json!({"path":"a\u{1b}[31m", "content":"x\r\ny\u{7}\n"}),
    )?;
    let (text, _) = present(&escaped, "tool write").ok_or("write missing")?;
    assert!(!text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\r'));
    Ok(())
}

#[test]
fn large_changes_are_counted_without_retaining_unbounded_diff_rows() -> Result {
    let content: String = (0..10_000).map(|n| format!("row {n}\n")).collect();
    let tool = tool(
        "write",
        serde_json::json!({"path":"large.rs", "content":content}),
    )?;
    let (text, marks) = present(&tool, "tool write").ok_or("large diff missing")?;
    assert!(text.contains("10000") && text.contains("9744"));
    assert_eq!(
        marks.iter().filter(|m| **m == Mark::Insert).count(),
        MAX_FILE_ROWS
    );
    assert!(marks.len() < 300 && text.len() < 16 * 1024);
    Ok(())
}

#[test]
fn edit_and_write_use_structured_final_diff_without_a_unified_patch() -> Result {
    for (name, input) in [
        (
            "edit",
            serde_json::json!({"path":"large.rs","old_text":"old","new_text":"requested"}),
        ),
        (
            "write",
            serde_json::json!({"path":"large.rs","content":"requested"}),
        ),
    ] {
        let mut tool = tool(name, input)?;
        if let DurableToolState::Completed { output, .. } = &mut tool.state {
            *output = serde_json::json!({"files":[{"relativePath":"large.rs","patchOmitted":false,
                "countsAvailable":true,"additions":2,"deletions":1,
                "diff":{"firstLine":601,"oldText":"before\n","newText":"formatted\nsecond\n","omittedOldLines":0,"omittedNewLines":0}}]}).to_string();
        }
        let (_, marks) = present(&tool, "tool").ok_or("final diff absent")?;
        assert_eq!(
            marks.iter().filter(|mark| **mark == Mark::Insert).count(),
            2
        );
        assert_eq!(
            marks.iter().filter(|mark| **mark == Mark::Delete).count(),
            1
        );
    }
    Ok(())
}
