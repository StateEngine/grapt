use super::*;
use std::fs;

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-child-budget-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&root)?;
        for child in ["config", "work", "service"] {
            fs::create_dir(root.join(child))?;
        }
        Ok(Self(root))
    }
    fn read(&self) -> Result<CatalogSources, String> {
        CatalogSources::read(&self.0.join("work"), CatalogEnvironment::isolated(&self.0))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn codex_v2_default_is_three_child_executions() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Scratch::new("default")?;
    assert_eq!(resolve(&fixture.read()?, true)?.get(), 3);
    Ok(())
}

#[test]
fn project_can_lower_but_never_raise_the_user_child_budget(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Scratch::new("narrow")?;
    fs::write(
        fixture.0.join("config/grapt.json"),
        r#"{"provider_capacity":{"children":7}}"#,
    )?;
    fs::write(
        fixture.0.join("work/grapt.json"),
        r#"{"provider_capacity":{"children":2}}"#,
    )?;
    assert_eq!(resolve(&fixture.read()?, false)?.get(), 7);
    assert_eq!(resolve(&fixture.read()?, true)?.get(), 2);
    fs::write(
        fixture.0.join("work/grapt.json"),
        r#"{"provider_capacity":{"children":64}}"#,
    )?;
    assert_eq!(resolve(&fixture.read()?, true)?.get(), 7);
    Ok(())
}

#[test]
fn invalid_child_budgets_are_errors_instead_of_defaults() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Scratch::new("invalid")?;
    for value in ["0", "65", "-1", "1.5", "\"three\""] {
        fs::write(
            fixture.0.join("config/grapt.json"),
            format!("{{\"provider_capacity\":{{\"children\":{value}}}}}"),
        )?;
        assert!(resolve(&fixture.read()?, true).is_err(), "accepted {value}");
    }
    Ok(())
}
