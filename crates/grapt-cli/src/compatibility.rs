//! The frozen oracle CLI's surfaces that Grapt refuses by name.
//!
//! Each entry is a row of `docs/cli-compatibility/MATRIX.md` classified
//! `refused`: the surface is outside the supported CLI contract (launch,
//! service, attach/resume, output modes and ACP), so it gets a stable refusal
//! with its reason and, where one exists, what to use instead. Never silence.

/// A refused TypeScript CLI surface and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub surface: &'static str,
    pub reason: &'static str,
}

const INTERACTIVE: Refusal = Refusal {
    surface: "grapt (interactive)",
    reason: "start the native terminal with grapt or grapt code",
};

const COMMANDS: &[Refusal] = &[
    Refusal {
        surface: "api",
        reason: "raw API requests go to the versioned HTTP API with an HTTP client; the CLI does not proxy them",
    },
    Refusal {
        surface: "auth",
        reason: "auth connect is unsupported; use grapt login <provider> --profile <profile> and grapt auth status",
    },
    Refusal {
        surface: "console",
        reason: "console access is not part of the supported CLI contract",
    },
    Refusal {
        surface: "debug",
        reason: "debugging tools are not part of the supported CLI contract",
    },
    Refusal {
        surface: "mcp",
        reason: "Use mcp add, list, remove, login or logout; mcp serve is not supported",
    },
    Refusal {
        surface: "migrate",
        reason: "data migration is the grapt-migrate binary",
    },
    Refusal {
        surface: "mini",
        reason: "start the native terminal with grapt or grapt code",
    },
    Refusal {
        surface: "pair",
        reason: "server pairing is not part of Grapt",
    },
    Refusal {
        surface: "plugin",
        reason: "use plugin marketplace add or plugin install; plugin listing and other lifecycle actions are not supported yet",
    },
    Refusal {
        surface: "serve",
        reason: "the product binary serves: grapt-server serve --service <root> --port 0, or grapt launch",
    },
    Refusal {
        surface: "upgrade",
        reason: "install a verified local package with grapt update --from <package-directory>",
    },
];

const SERVICE_ACTIONS: &[Refusal] = &[
    Refusal {
        surface: "service get",
        reason: "service configuration is the service root's service.conf, owned by the product",
    },
    Refusal {
        surface: "service restart",
        reason: "the elected service owner replaces the service; the CLI is its client",
    },
    Refusal {
        surface: "service set",
        reason: "service configuration is the service root's service.conf, owned by the product",
    },
    Refusal {
        surface: "service stop",
        reason: "the elected service owner stops the service; the CLI is its client",
    },
    Refusal {
        surface: "service unset",
        reason: "service configuration is the service root's service.conf, owned by the product",
    },
];

const FLAGS: &[(&str, Refusal)] = &[
    (
        "-f",
        Refusal {
            surface: "--file",
            reason: "file attachments are not part of the CLI contract",
        },
    ),
    (
        "--file",
        Refusal {
            surface: "--file",
            reason: "file attachments are not part of the CLI contract",
        },
    ),
    (
        "--standalone",
        Refusal {
            surface: "--standalone",
            reason: "a private server is --embedded --service <root> --product <binary>",
        },
    ),
    (
        "--server",
        Refusal {
            surface: "--server",
            reason: "a server is --remote <https-url> --credential <file>, or --service <root> for the local service",
        },
    ),
];

/// `--format json` streamed live, advisory events. Grapt's machine output is
/// durable truth instead.
pub const FORMAT_JSON: Refusal = Refusal {
    surface: "--format json",
    reason: "machine output is durable, not a projection of advisory live events: use --jsonl",
};

/// The refusal for an oracle command, or for `service <action>`.
pub fn command(name: &str, action: Option<&str>) -> Option<Refusal> {
    if name == "service" {
        let action = action?;
        return SERVICE_ACTIONS
            .iter()
            .find(|refusal| refusal.surface.strip_prefix("service ") == Some(action))
            .copied();
    }
    COMMANDS
        .iter()
        .find(|refusal| refusal.surface == name)
        .copied()
}

/// The refusal for an oracle flag.
pub fn flag(name: &str) -> Option<Refusal> {
    FLAGS
        .iter()
        .find(|(flag, _)| *flag == name)
        .map(|(_, refusal)| *refusal)
}

/// The oracle's default command started the interactive client.
pub const fn interactive() -> Refusal {
    INTERACTIVE
}

/// Every refusal, for the compatibility matrix and its checker.
pub fn all() -> impl Iterator<Item = Refusal> {
    COMMANDS
        .iter()
        .chain(SERVICE_ACTIONS)
        .copied()
        .chain(FLAGS.iter().map(|(_, refusal)| *refusal))
        .chain([FORMAT_JSON, INTERACTIVE])
}
