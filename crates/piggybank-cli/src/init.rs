// init.rs — `piggybank init` subcommand
//
// One command that:
//   (a) ensures ~/.piggybank/hooks/ exists and contains the hook scripts
//   (b) merges MCP server entry into ~/.claude.json mcpServers
//   (c) merges hook entries into ~/.claude/settings.json PostToolUse+PreCompact
//   (d) prints what changed
//
// Supports --dry-run and --uninstall.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

// Hook scripts embedded at compile time so the binary is self-contained.
// Paths are relative to this source file: crates/piggybank-cli/src/init.rs
// -> ../../../hooks/ -> workspace root/hooks/
const HOOK_POST_TOOL_USE: &str = include_str!("../../../hooks/post-tool-use-compress.sh");
const HOOK_PRE_COMPACT: &str = include_str!("../../../hooks/pre-compact-budget.sh");
const HOOKS_JSON: &str = include_str!("../../../hooks/hooks.json");

pub struct InitOptions {
    pub dry_run: bool,
    pub uninstall: bool,
    pub store_dir: Option<String>,
    /// Override source directory for hook scripts (e.g. bundled in npm package).
    pub hooks_src: Option<PathBuf>,
}

impl InitOptions {
    pub fn from_args(args: &[String]) -> Self {
        let dry_run = args.iter().any(|a| a == "--dry-run");
        let uninstall = args.iter().any(|a| a == "--uninstall");
        let store_dir = args
            .iter()
            .position(|a| a == "--store-dir")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let hooks_src = args
            .iter()
            .position(|a| a == "--hooks-src")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from);
        InitOptions {
            dry_run,
            uninstall,
            store_dir,
            hooks_src,
        }
    }
}

pub fn run_init(args: &[String]) -> std::process::ExitCode {
    let opts = InitOptions::from_args(args);
    let home = match home_dir() {
        Some(h) => h,
        None => {
            eprintln!("error: cannot determine home directory");
            return std::process::ExitCode::FAILURE;
        }
    };

    let piggybank_dir = home.join(".piggybank");
    let hooks_dir = piggybank_dir.join("hooks");
    let store_dir = opts
        .store_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| piggybank_dir.join("store"));
    let claude_json = home.join(".claude.json");
    let settings_json = home.join(".claude").join("settings.json");

    if opts.uninstall {
        return run_uninstall(&opts, &hooks_dir, &claude_json, &settings_json);
    }

    let mut changed: Vec<String> = Vec::new();

    // Step (a): install hook scripts to ~/.piggybank/hooks/
    if let Err(e) = install_hooks(&opts, &hooks_dir, opts.hooks_src.as_deref(), &mut changed) {
        eprintln!("error installing hooks: {e}");
        return std::process::ExitCode::FAILURE;
    }

    // Step (b): merge MCP server entry into ~/.claude.json
    let mcp_command = home
        .join(".axe")
        .join("bin")
        .join("piggybank")
        .to_string_lossy()
        .into_owned();
    if let Err(e) = merge_claude_json(
        &opts,
        &claude_json,
        &mcp_command,
        store_dir.to_string_lossy().as_ref(),
        &mut changed,
    ) {
        eprintln!("error updating {}: {e}", claude_json.display());
        return std::process::ExitCode::FAILURE;
    }

    // Step (c): merge hook entries into ~/.claude/settings.json
    let hook_command = format!(
        "{}/hooks/post-tool-use-compress.sh",
        hooks_dir.to_string_lossy()
    );
    let pre_compact_command = format!(
        "{}/hooks/pre-compact-budget.sh",
        hooks_dir.to_string_lossy()
    );
    if let Err(e) = merge_settings_json(
        &opts,
        &settings_json,
        &hook_command,
        &pre_compact_command,
        &mut changed,
    ) {
        eprintln!("error updating {}: {e}", settings_json.display());
        return std::process::ExitCode::FAILURE;
    }

    // Step (d): report
    if changed.is_empty() {
        println!("piggybank init: nothing to do (already configured)");
    } else {
        println!(
            "piggybank init: {}",
            if opts.dry_run {
                "would change"
            } else {
                "changed"
            }
        );
        for c in &changed {
            println!("  {c}");
        }
    }

    std::process::ExitCode::SUCCESS
}

fn run_uninstall(
    opts: &InitOptions,
    hooks_dir: &Path,
    claude_json: &Path,
    settings_json: &Path,
) -> std::process::ExitCode {
    let mut changed: Vec<String> = Vec::new();

    // Remove hook scripts
    for name in &["post-tool-use-compress.sh", "pre-compact-budget.sh"] {
        let path = hooks_dir.join(name);
        if path.exists() {
            if !opts.dry_run {
                if let Err(e) = fs::remove_file(&path) {
                    eprintln!("error removing {}: {e}", path.display());
                }
            }
            changed.push(format!("removed {}", path.display()));
        }
    }

    // Remove MCP entry from ~/.claude.json
    if claude_json.exists() {
        if let Ok(text) = fs::read_to_string(claude_json) {
            if let Ok(mut val) = serde_json::from_str::<Value>(&text) {
                if let Some(servers) = val.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
                    if servers.remove("piggybank").is_some() {
                        changed.push(format!(
                            "removed mcpServers.piggybank from {}",
                            claude_json.display()
                        ));
                        if !opts.dry_run {
                            let _ = backup_file(claude_json);
                            let _ = write_json(claude_json, &val);
                        }
                    }
                }
            }
        }
    }

    // Remove hook entries from ~/.claude/settings.json
    if settings_json.exists() {
        if let Ok(text) = fs::read_to_string(settings_json) {
            if let Ok(mut val) = serde_json::from_str::<Value>(&text) {
                let mut modified = false;
                for event in &["PostToolUse", "PreCompact"] {
                    if let Some(hooks_arr) = val
                        .get_mut("hooks")
                        .and_then(|h| h.get_mut(*event))
                        .and_then(|v| v.as_array_mut())
                    {
                        let before = hooks_arr.len();
                        hooks_arr.retain(|entry| {
                            let cmd = entry
                                .get("hooks")
                                .and_then(|h| h.get(0))
                                .and_then(|h| h.get("command"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("");
                            !cmd.contains("piggybank")
                        });
                        if hooks_arr.len() < before {
                            modified = true;
                            changed.push(format!(
                                "removed piggybank {event} hook from {}",
                                settings_json.display()
                            ));
                        }
                    }
                }
                if modified && !opts.dry_run {
                    let _ = backup_file(settings_json);
                    let _ = write_json(settings_json, &val);
                }
            }
        }
    }

    if changed.is_empty() {
        println!("piggybank uninstall: nothing to remove");
    } else {
        for c in &changed {
            println!(
                "{}",
                if opts.dry_run {
                    format!("would: {c}")
                } else {
                    c.clone()
                }
            );
        }
    }
    std::process::ExitCode::SUCCESS
}

fn install_hooks(
    opts: &InitOptions,
    hooks_dir: &Path,
    hooks_src: Option<&Path>,
    changed: &mut Vec<String>,
) -> std::io::Result<()> {
    if !opts.dry_run {
        fs::create_dir_all(hooks_dir)?;
    }

    let embedded: &[(&str, &str)] = &[
        ("post-tool-use-compress.sh", HOOK_POST_TOOL_USE),
        ("pre-compact-budget.sh", HOOK_PRE_COMPACT),
        ("hooks.json", HOOKS_JSON),
    ];

    for (name, embedded_content) in embedded {
        let dest = hooks_dir.join(name);
        // Prefer hooks_src if it exists and contains this file
        let content = if let Some(src_dir) = hooks_src {
            let src_path = src_dir.join(name);
            if src_path.exists() {
                fs::read_to_string(&src_path).unwrap_or_else(|_| embedded_content.to_string())
            } else {
                embedded_content.to_string()
            }
        } else {
            embedded_content.to_string()
        };

        let needs_write = if dest.exists() {
            fs::read_to_string(&dest).unwrap_or_default() != content
        } else {
            true
        };
        if needs_write {
            changed.push(format!("wrote {}", dest.display()));
            if !opts.dry_run {
                fs::write(&dest, &content)?;
                if name.ends_with(".sh") {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn merge_claude_json(
    opts: &InitOptions,
    path: &Path,
    mcp_command: &str,
    store_dir: &str,
    changed: &mut Vec<String>,
) -> std::io::Result<()> {
    let mut val: Value = if path.exists() {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let servers = val
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .unwrap()
        .entry("piggybank")
        .or_insert_with(|| json!(null));

    let desired = json!({
        "command": mcp_command,
        "args": ["mcp", "serve", "--store-dir", store_dir],
        "type": "stdio"
    });

    if servers.as_null().is_some() || *servers != desired {
        changed.push(format!("set mcpServers.piggybank in {}", path.display()));
        *servers = desired;
        if !opts.dry_run {
            backup_file(path)?;
            write_json(path, &val)?;
        }
    }
    Ok(())
}

fn merge_settings_json(
    opts: &InitOptions,
    path: &Path,
    post_tool_cmd: &str,
    pre_compact_cmd: &str,
    changed: &mut Vec<String>,
) -> std::io::Result<()> {
    let mut val: Value = if path.exists() {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let hooks = val
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .unwrap();

    // PostToolUse hook
    {
        let post_arr = hooks
            .entry("PostToolUse")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();

        let desired_entry = json!({
            "matcher": ".*",
            "hooks": [{"type": "command", "command": post_tool_cmd}]
        });

        let already = post_arr.iter().any(|e| {
            e.get("hooks")
                .and_then(|h| h.get(0))
                .and_then(|h| h.get("command"))
                .and_then(|c| c.as_str())
                == Some(post_tool_cmd)
        });
        if !already {
            post_arr.push(desired_entry);
            changed.push(format!(
                "added PostToolUse piggybank hook to {}",
                path.display()
            ));
        }
    }

    // PreCompact hook
    {
        let pre_arr = hooks
            .entry("PreCompact")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();

        let desired_entry = json!({
            "hooks": [{"type": "command", "command": pre_compact_cmd}]
        });

        let already = pre_arr.iter().any(|e| {
            e.get("hooks")
                .and_then(|h| h.get(0))
                .and_then(|h| h.get("command"))
                .and_then(|c| c.as_str())
                == Some(pre_compact_cmd)
        });
        if !already {
            pre_arr.push(desired_entry);
            changed.push(format!(
                "added PreCompact piggybank hook to {}",
                path.display()
            ));
        }
    }

    if changed
        .iter()
        .any(|c| c.contains(path.to_str().unwrap_or("")))
        && !opts.dry_run
    {
        if path.parent().is_some() {
            fs::create_dir_all(path.parent().unwrap())?;
        }
        backup_file(path)?;
        write_json(path, &val)?;
    }

    Ok(())
}

fn backup_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.piggybank-backup");
        fs::copy(path, backup)?;
    }
    Ok(())
}

fn write_json(path: &Path, val: &Value) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(val)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text + "\n")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}
