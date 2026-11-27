use super::*;

#[test]
fn held_highlighting_keeps_requests_nonblocking_and_bounded() -> Result<(), String> {
    let (requests, input) = mpsc::sync_channel(MAX_PENDING);
    let (output, results) = mpsc::sync_channel(MAX_PENDING);
    let mut highlighter = Highlighter {
        worker: Some(Worker { requests, results }),
        ..Highlighter::default()
    };
    let palette = Palette::new(false);
    // Nothing consumes the requests: a blocking submission would hang here.
    for index in 0..100 {
        assert!(highlighter
            .rows("rust", &format!("let x = {index};\n"), palette)
            .is_none());
    }
    assert_eq!(highlighter.pending.len(), MAX_PENDING);
    let first = input.try_recv().map_err(|e| e.to_string())?;
    assert!(highlighter.rows("rust", &first.source, palette).is_none());
    assert_eq!(
        highlighter.pending.len(),
        MAX_PENDING,
        "duplicate job stays coalesced"
    );
    let colored = vec![Line::styled(
        "let x = 0;",
        palette.normal().fg(palette.accent),
    )];
    output
        .try_send((first.clone(), colored.clone()))
        .map_err(|e| e.to_string())?;
    assert!(highlighter.refresh()?);
    assert_eq!(
        highlighter.rows("rust", &first.source, palette),
        Some(colored)
    );
    assert!(
        highlighter
            .rows("rust", &first.source, Palette::new(true))
            .is_none(),
        "theme changes never reuse old colors"
    );
    assert_eq!(highlighter.pending.len(), MAX_PENDING);
    Ok(())
}

#[test]
fn completed_highlighting_bounds_cache_and_reports_a_stopped_worker() -> Result<(), String> {
    let (requests, input) = mpsc::sync_channel(MAX_PENDING);
    let (output, results) = mpsc::sync_channel(MAX_PENDING);
    let mut highlighter = Highlighter {
        worker: Some(Worker { requests, results }),
        ..Highlighter::default()
    };
    let palette = Palette::new(false);
    for index in 0..100 {
        highlighter.new_view();
        let text = format!("let x = {index};\n");
        assert!(highlighter.rows("rust", &text, palette).is_none());
        let key = input.try_recv().map_err(|e| e.to_string())?;
        output
            .try_send((key, plain(&text, palette)))
            .map_err(|e| e.to_string())?;
        assert!(highlighter.refresh()?);
        assert!(highlighter.cached.len() <= MAX_CACHED);
    }
    assert_eq!(highlighter.cached.len(), MAX_CACHED);
    assert!(highlighter.pending.is_empty());
    drop(output);
    assert!(highlighter.refresh().is_err(), "worker failure is visible");
    assert!(
        !highlighter.refresh()?,
        "one stopped worker does not schedule repeated frames"
    );
    assert!(highlighter.rows("rust", "new source", palette).is_none());
    Ok(())
}

#[test]
fn seventeen_fences_settle_without_completion_renewing_work() -> Result<(), String> {
    let (requests, input) = mpsc::sync_channel(MAX_PENDING);
    let (output, results) = mpsc::sync_channel(MAX_PENDING);
    let mut highlighter = Highlighter {
        worker: Some(Worker { requests, results }),
        ..Highlighter::default()
    };
    let palette = Palette::new(false);
    let mut jobs = 0;
    for _ in 0..100 {
        for index in 0..17 {
            let _ = highlighter.rows("rust", &format!("let x = {index};\n"), palette);
        }
        for _ in 0..MAX_PENDING {
            let Ok(key) = input.try_recv() else {
                break;
            };
            let rows = plain(&key.source, palette);
            output.try_send((key, rows)).map_err(|e| e.to_string())?;
            jobs += 1;
        }
        if !highlighter.refresh()? {
            break;
        }
    }
    assert_eq!(jobs, MAX_CACHED);
    assert!(highlighter.pending.is_empty());
    assert!(!highlighter.refresh()?);
    assert!(input.try_recv().is_err(), "idle view has no further work");
    highlighter.new_view();
    assert!(highlighter
        .rows("rust", "a newly visible fence\n", palette)
        .is_none());
    assert!(
        input.try_recv().is_ok(),
        "navigation admits new coloring work"
    );
    Ok(())
}

#[test]
fn worker_panic_closes_results_before_waking_an_idle_ui() -> Result<(), String> {
    let (wake, ready) = mpsc::sync_channel(1);
    let (requests, _input) = mpsc::sync_channel(MAX_PENDING);
    let (output, results) = mpsc::sync_channel(MAX_PENDING);
    let mut highlighter = Highlighter {
        worker: Some(Worker { requests, results }),
        ..Highlighter::default()
    };
    highlighter.set_wake(wake.clone());
    let thread = std::thread::spawn(move || {
        let _exit = NotifyOnExit(Some(wake));
        let _output = output;
        panic!("injected highlighter panic");
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    assert!(
        highlighter.refresh().is_err(),
        "the exit wake delivers failure without input"
    );
    assert!(thread.join().is_err(), "the intended worker panic executed");
    assert!(
        !highlighter.refresh()?,
        "failure does not keep waking the UI"
    );
    Ok(())
}

#[test]
fn lazy_grammar_closures_preserve_the_full_bundle_colors_and_language_aliases() -> Result<(), String>
{
    let full = two_face::syntax::extra_newlines();
    let themes = ThemeSet::load_defaults();
    let packed = theme(false)?;
    assert_eq!(packed.themes.len(), 1);
    assert_eq!(theme(true)?.themes.len(), 1);
    let mut grammars = grammars::Grammars::new()?;
    let cases = [
        (
            "python",
            "def f(x: int):\n    return f\"value={x + 1}\" # comment\n",
        ),
        (
            "RUST",
            "fn main() { let x = Some(42); println!(\"{x:?}\"); }\n",
        ),
        ("js", "const f = (x) => `value ${x + 1}`; // note\n"),
        ("ts", "interface X { n: number }; const x: X = { n: 1 };\n"),
        (
            "html",
            "<style>a { color: red; }</style>\n<script>let x = 1;</script>\n",
        ),
        ("json", "{\"ok\": true, \"n\": 12, \"s\": \"中\"}\n"),
        ("sql", "SELECT name, COUNT(*) FROM users WHERE id > 1;\n"),
        ("sh", "#!/bin/sh\nfor x in a b; do echo \"${x}\"; done\n"),
        (
            "cpp",
            "#include <vector>\ntemplate<class T> struct X { T v; };\n",
        ),
        (
            "markdown",
            "# Heading\n**bold** and `code`\n```python\ndef f(): pass\n```\n",
        ),
        ("yaml", "key: value\nlist:\n  - true\n  - 42\n"),
        ("css", "@media screen { a:hover { color: #abcdef; } }\n"),
        ("unknown-language", "unchanged plain 中👩🏽‍💻 text\n"),
    ];
    for (language, source) in cases {
        for light in [false, true] {
            let packed = theme(light)?;
            let key = Key {
                language: language.into(),
                source: source.into(),
                light,
            };
            let loaded = grammars.get(language)?;
            assert_eq!(
                render(&key, loaded, &packed),
                render(&key, &full, &themes),
                "{language} light={light}"
            );
        }
    }
    for language in full.syntaxes().iter().map(|syntax| &syntax.name) {
        let key = Key {
            language: language.clone(),
            source: "value = 42; \"text\" # comment\n".into(),
            light: false,
        };
        assert_eq!(
            render(&key, grammars.get(language)?, &packed),
            render(&key, &full, &themes),
            "language {language}"
        );
    }
    assert!(
        grammars.get("python")?.syntaxes().len() < 16,
        "one small language does not load unrelated grammars"
    );
    assert_eq!(grammars.get("unknown-language")?.syntaxes().len(), 1);
    Ok(())
}

#[test]
fn queued_highlights_cross_batch_boundaries_and_accept_later_work() -> Result<(), String> {
    let (wake, ready) = mpsc::sync_channel(1);
    let mut highlighter = Highlighter::default();
    highlighter.set_wake(wake);
    let full = two_face::syntax::extra_newlines();
    let themes = ThemeSet::load_defaults();
    let keys: Vec<_> = (0..8)
        .map(|index| Key {
            language: "python".into(),
            source: format!("def fence_{index}(x):\n    return x + {index}\n"),
            light: index % 2 == 0,
        })
        .collect();
    // Reference parsing is not part of the asynchronous worker wait.
    let expected: Vec<_> = keys.iter().map(|key| render(key, &full, &themes)).collect();
    for end in [7, 8] {
        let mut complete = false;
        for _ in 0..100 {
            highlighter.refresh()?;
            complete = true;
            for (key, expected) in keys[..end].iter().zip(&expected) {
                if let Some(rows) =
                    highlighter.rows(&key.language, &key.source, Palette::new(key.light))
                {
                    assert_eq!(&rows, expected);
                } else {
                    complete = false;
                }
            }
            if complete {
                break;
            }
            // Wakes coalesce; a quiet interval is not a completion boundary.
            match ready.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("highlight wake channel disconnected".into());
                }
            }
        }
        assert!(complete, "every queued or newly admitted fence completes");
        assert!(highlighter.pending.is_empty());
    }
    Ok(())
}
