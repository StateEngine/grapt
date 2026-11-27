use grapt_sandbox::{ContainedShellSpawner, NetworkAccess, Policy, Seatbelt, ShellRequest};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn contents(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn wait(child: &mut Child) -> Result<()> {
    for _ in 0..600 {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("fixture preparation failed: {status}").into())
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill()?;
    child.wait()?;
    Err("fixture preparation exceeded one minute".into())
}

fn host(directory: &Path, program: &str, arguments: &[&str]) -> Result<()> {
    wait(
        &mut Command::new(program)
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(arguments)
            .spawn()?,
    )
}

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    common: PathBuf,
    temp: PathBuf,
    declared: PathBuf,
    helper: PathBuf,
    environment: Vec<String>,
    policy: Policy,
}

impl Fixture {
    fn prepare() -> Result<Self> {
        let root =
            std::env::temp_dir().join(format!("grapt-seatbelt-native-{}", std::process::id()));
        fs::create_dir(&root)?;
        let root = fs::canonicalize(root)?;
        let workspace = root.join("workspace");
        let common = root.join("common");
        let temp = root.join("temp");
        let declared = root.join("declared");
        for directory in [&common, &temp, &declared, &root.join("home")] {
            fs::create_dir(directory)?;
        }
        host(&common, "git", &["init", "--initial-branch=main"])?;
        host(&common, "git", &["config", "user.name", "Seatbelt fixture"])?;
        host(
            &common,
            "git",
            &["config", "user.email", "seatbelt@example.invalid"],
        )?;
        fs::write(common.join("initial"), "initial\n")?;
        host(&common, "git", &["add", "initial"])?;
        host(&common, "git", &["commit", "-m", "Initial fixture"])?;
        host(
            &common,
            "git",
            &[
                "worktree",
                "add",
                "-b",
                "contained",
                workspace.to_str().ok_or("non-Unicode fixture")?,
            ],
        )?;
        fs::create_dir_all(workspace.join("cargo/src"))?;
        fs::write(
            workspace.join("cargo/Cargo.toml"),
            "[package]\nname='seatbelt-fixture'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        fs::write(
            workspace.join("cargo/src/main.rs"),
            "fn main() {}\n#[test] fn arithmetic() { assert_eq!(2 + 2, 4); }\n",
        )?;
        fs::create_dir(workspace.join("npm"))?;
        fs::create_dir(workspace.join("npm/dep"))?;
        fs::write(
            workspace.join("npm/dep/package.json"),
            r#"{"name":"seatbelt-dep","version":"1.0.0"}"#,
        )?;
        fs::write(
            workspace.join("npm/package.json"),
            r#"{"name":"seatbelt-fixture","version":"1.0.0","dependencies":{"seatbelt-dep":"file:dep"},"scripts":{"postinstall":"node -e \"require('fs').writeFileSync('postinstall-ok','ok')\""}}"#,
        )?;
        host(
            &workspace.join("npm"),
            "npm",
            &[
                "install",
                "--package-lock-only",
                "--ignore-scripts",
                "--offline",
                "--no-audit",
                "--no-fund",
                "--cache",
                temp.to_str().ok_or("non-Unicode temp")?,
            ],
        )?;
        let helper = std::env::current_exe()?;
        let mut protected = vec![
            workspace.join(".git"),
            common.join(".git/config"),
            common.join(".git/hooks"),
        ];
        for name in ["grapt.jsonc", "grapt-server", "grapt-shim"] {
            let path = declared.join(name);
            fs::write(&path, "original")?;
            protected.push(path);
        }
        protected.push(workspace.join("missing-parent/config"));
        fs::write(root.join("outside"), "original")?;
        fs::write(root.join("secret"), "original")?;
        let mut environment = vec![
            format!("PATH={}", std::env::var("PATH")?),
            format!("HOME={}", root.join("home").display()),
            format!("TMPDIR={}/", temp.display()),
            format!("CARGO_HOME={}", temp.join("cargo-home").display()),
            format!(
                "CARGO_TARGET_DIR={}",
                workspace.join("cargo/target").display()
            ),
            format!(
                "RUSTUP_HOME={}",
                std::env::var("RUSTUP_HOME")
                    .map(PathBuf::from)
                    .or_else(
                        |_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".rustup"))
                    )?
                    .display()
            ),
            "RUSTUP_TOOLCHAIN=1.97.1".into(),
            "GIT_CONFIG_NOSYSTEM=1".into(),
            "GIT_CONFIG_GLOBAL=/dev/null".into(),
        ];
        for key in ["SDKROOT", "DEVELOPER_DIR"] {
            if let Ok(value) = std::env::var(key) {
                environment.push(format!("{key}={value}"));
            }
        }
        let policy = Policy {
            readable_roots: vec!["/".into()],
            writable_roots: vec![
                workspace.clone(),
                common.clone(),
                temp.clone(),
                declared.clone(),
            ],
            protected_paths: protected,
            denied_paths: vec![root.join("secret")],
            network: NetworkAccess::Off,
        };
        Ok(Self {
            root,
            workspace,
            common,
            temp,
            declared,
            helper,
            environment,
            policy,
        })
    }

    fn run(
        &self,
        label: &str,
        source: String,
        expected: i32,
        network: NetworkAccess,
    ) -> Result<()> {
        let mut policy = self.policy.clone();
        policy.network = network;
        let request = ShellRequest {
            command_line: source,
            working_directory: self.workspace.clone(),
            environment: self.environment.clone(),
            timeout_ms: 60_000,
            policy,
        };
        let mut child = Seatbelt::new(self.helper.clone()).spawn(&request)?;
        let enforcement = child.enforcement();
        for _ in 0..600 {
            if let Some(code) = child.try_wait()? {
                println!("{label}: {enforcement:?}, exit={code}");
                if expected == -1 && code == 0 {
                    return Err(format!("SEATBELT_ESCAPE: {label}").into());
                }
                if (expected == -1 && code != 0) || code == expected {
                    return Ok(());
                }
                return Err(format!("{label}: expected exit {expected}, got {code}").into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        child.kill()?;
        child.wait()?;
        Err(format!("{label}: command exceeded one minute").into())
    }

    fn prove(&self) -> Result<()> {
        self.run(
            "cargo-build-test",
            "cd cargo && cargo build --offline && cargo test --offline".into(),
            0,
            NetworkAccess::Off,
        )?;
        self.run("npm-ci-postinstall", format!("cd npm && npm ci --offline --no-audit --no-fund --cache {} && test -f postinstall-ok", quoted(&self.temp)), 0, NetworkAccess::Off)?;
        self.run(
            "git-commit",
            "printf allowed > allowed && git add allowed && git commit -m contained".into(),
            0,
            NetworkAccess::Off,
        )?;
        for (label, path) in [
            ("git-config", self.common.join(".git/config")),
            ("git-hooks", self.common.join(".git/hooks/new-hook")),
            ("own-config", self.declared.join("grapt.jsonc")),
            ("own-binary", self.declared.join("grapt-server")),
            ("own-shim", self.declared.join("grapt-shim")),
            ("outside", self.root.join("outside")),
        ] {
            let before = contents(&path)?;
            self.run(
                label,
                format!("printf changed > {}", quoted(&path)),
                -1,
                NetworkAccess::Off,
            )?;
            if contents(&path)? != before {
                return Err(format!("SEATBELT_ESCAPE: {label} changed bytes").into());
            }
        }
        let config = self.common.join(".git/config");
        for (label, flag) in [("protected-hardlink", ""), ("protected-symlink", "-s")] {
            let alias = self.workspace.join(label);
            let before = contents(&config)?;
            let result = self.run(
                label,
                format!(
                    "ln {flag} {} {} && printf changed > {}",
                    quoted(&config),
                    quoted(&alias),
                    quoted(&alias)
                ),
                -1,
                NetworkAccess::Off,
            );
            // Remove only the owned alias before the next preparation check.
            match fs::remove_file(&alias) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            result?;
            if contents(&config)? != before {
                return Err(format!("SEATBELT_ESCAPE: {label} changed protected bytes").into());
            }
        }
        self.run(
            "missing-config-parent",
            "mkdir missing-parent && printf changed > missing-parent/config".into(),
            -1,
            NetworkAccess::Off,
        )?;
        if self.workspace.join("missing-parent").exists() {
            return Err("SEATBELT_ESCAPE: missing config parent created".into());
        }
        self.run(
            "git-relocation",
            format!(
                "mv {} {}",
                quoted(&self.common.join(".git")),
                quoted(&self.common.join("moved-git"))
            ),
            -1,
            NetworkAccess::Off,
        )?;
        self.run(
            "denied-read",
            format!("cat {}", quoted(&self.root.join("secret"))),
            -1,
            NetworkAccess::Off,
        )?;
        self.run(
            "symlink-escape",
            format!(
                "ln -s {} escape-link && printf changed > escape-link",
                quoted(&self.root.join("outside"))
            ),
            -1,
            NetworkAccess::Off,
        )?;
        if fs::read_to_string(self.root.join("outside"))? != "original" {
            return Err("SEATBELT_ESCAPE: symlink changed outside bytes".into());
        }
        for path in [&self.temp, &self.declared, &self.common] {
            self.run(
                "declared-write",
                format!("printf allowed > {}", quoted(&path.join("allowed"))),
                0,
                NetworkAccess::Off,
            )?;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let source = format!(
            "{} --network-probe {}",
            quoted(&self.helper),
            listener.local_addr()?.port()
        );
        self.run("reachable-network-on", source.clone(), 0, NetworkAccess::On)?;
        self.run("access-denied-network-off", source, 77, NetworkAccess::Off)?;
        self.run("native-exit", "exit 23".into(), 23, NetworkAccess::Off)?;
        Ok(())
    }
}

pub fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(result) = Seatbelt::inner_main(&arguments) {
        return result.map_err(Into::into);
    }
    if let [mode, port] = arguments.as_slice() {
        if mode == "--network-probe" {
            let address = format!("127.0.0.1:{port}").parse()?;
            let code = match std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1))
            {
                Ok(_) => 0,
                Err(error) if matches!(error.raw_os_error(), Some(1 | 13)) => 77,
                Err(_) => 79,
            };
            std::process::exit(code);
        }
    }
    if !arguments.is_empty() {
        return Err("unknown Seatbelt smoke arguments".into());
    }
    let fixture = Fixture::prepare()?;
    fixture.prove()?;
    fs::remove_dir_all(&fixture.root)?;
    println!("Seatbelt native cargo/npm/git and filesystem/network denials passed");
    Ok(())
}
