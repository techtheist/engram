//! Writing the MCP server entry into each client's config file, repairing what's there.

use super::*;

impl Setup {
    /// v0.3.0 → v0.4.0 repair: if a JSON config's engram entry launches the
    /// pre-rename binary, re-point it at this one. Handles both shapes —
    /// `mcpServers.engram.command` (claude/gemini) and the opencode/kilo
    /// `mcp.engram.command` array. Returns the rewritten file when a repair
    /// applied; None means nothing to fix.
    pub(super) fn repaired_json_config(&self, raw: &str) -> Option<String> {
        let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let mut repaired = false;
        if let Some(entry) = v.pointer_mut("/mcpServers/engram") {
            let cmd = entry.get("command").and_then(|c| c.as_str());
            if cmd.is_some_and(is_prerename_bin) {
                entry["command"] = serde_json::json!(self.bin);
                entry["args"] = serde_json::json!(["mcp", "--db", self.db]);
                repaired = true;
            }
        }
        if let Some(entry) = v.pointer_mut("/mcp/engram") {
            let first = entry
                .get("command")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str());
            if first.is_some_and(is_prerename_bin) {
                entry["command"] = serde_json::json!([self.bin, "mcp", "--db", self.db]);
                repaired = true;
            }
        }
        repaired
            .then(|| serde_json::to_string_pretty(&v).ok())
            .flatten()
            .map(|s| s + "\n")
    }

    /// The same repair for codex's global TOML: rewrite the `command = "…"`
    /// line inside `[mcp_servers.engram]` when it names the pre-rename binary.
    pub(super) fn repaired_codex_toml(&self, raw: &str) -> Option<String> {
        let mut in_engram = false;
        let mut repaired = false;
        let out: Vec<String> = raw
            .lines()
            .map(|line| {
                let t = line.trim();
                if t.starts_with('[') {
                    in_engram = t == "[mcp_servers.engram]";
                } else if in_engram
                    && t.starts_with("command")
                    && t.split('"').nth(1).is_some_and(is_prerename_bin)
                {
                    repaired = true;
                    return format!("command = {}", toml_str(&self.bin));
                }
                line.to_string()
            })
            .collect();
        repaired.then(|| out.join("\n") + "\n")
    }

    /// The launch arguments an adapter writes. db-less (`["mcp"]`) is the
    /// 0.8.8 default wherever the client launches the server with the
    /// project as its cwd — the bridge binds by MCP roots, then cwd, so the
    /// entry is portable and one shape serves every repo. Adapters whose
    /// launch cwd is unverified (IDEs mostly) keep the explicit --db.
    fn mcp_args_json(&self, with_db: bool) -> String {
        if with_db {
            format!("[\"mcp\", \"--db\", {}]", json_str(&self.db))
        } else {
            "[\"mcp\"]".to_string()
        }
    }

    pub(super) fn mcp_snippet(&self, with_db: bool) -> String {
        format!(
            "\"engram\": {{ \"command\": {}, \"args\": {} }}",
            json_str(&self.bin),
            self.mcp_args_json(with_db)
        )
    }

    /// Write an `mcpServers`-shaped config, or print the snippet when a
    /// foreign config already exists (never rewrite user JSON blindly).
    pub(super) fn write_mcp_servers(
        &self,
        rel: &str,
        label: &str,
        with_db: bool,
    ) -> anyhow::Result<()> {
        let path = self.repo.join(rel);
        if path.exists() {
            let current = fs::read_to_string(&path)?;
            if current.contains("\"engram\"") {
                if let Some(next) = self.repaired_json_config(&current) {
                    fs::write(&path, next)?;
                    say(&format!(
                        "{label}: re-pointed {rel}'s engram entry at this binary (was the pre-rename `engram`)"
                    ));
                } else {
                    say(&format!("{label}: {rel} already has engram — leaving it"));
                }
            } else {
                say(&format!(
                    "{label}: {rel} exists — add this to its mcpServers manually:"
                ));
                println!("    {}", self.mcp_snippet(with_db));
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &path,
            format!(
                "{{\n  \"mcpServers\": {{\n    \"engram\": {{\n      \"command\": {},\n      \"args\": {}\n    }}\n  }}\n}}\n",
                json_str(&self.bin),
                self.mcp_args_json(with_db)
            ),
        )?;
        say(&format!("{label}: wrote {rel}"));
        Ok(())
    }

    /// Merge the db-less engram entry into one Windsurf global config, keeping
    /// other servers. Unparseable JSON gets the snippet printed instead.
    pub(super) fn wire_windsurf_file(&self, path: &Path, shown: &str) -> anyhow::Result<()> {
        let entry = serde_json::json!({ "command": self.bin, "args": ["mcp"] });
        match fs::read_to_string(path) {
            // An empty file (a fresh Windsurf plugin leaves one, issue #18)
            // is an empty config, not a broken one.
            Ok(current) => {
                match serde_json::from_str::<serde_json::Value>(if current.trim().is_empty() {
                    "{}"
                } else {
                    &current
                }) {
                    Ok(mut v) if v.is_object() => {
                        if let Some(engram) = v.pointer_mut("/mcpServers/engram") {
                            let cmd = engram.get("command").and_then(|c| c.as_str());
                            if cmd.is_some_and(is_prerename_bin) {
                                // The global repair is db-less on purpose — a
                                // --db here would pin every project to one repo.
                                engram["command"] = serde_json::json!(self.bin);
                                engram["args"] = serde_json::json!(["mcp"]);
                                fs::write(path, serde_json::to_string_pretty(&v)? + "\n")?;
                                say(&format!(
                                    "windsurf: re-pointed {shown}'s engram entry at this binary (was the pre-rename `engram`)"
                                ));
                            } else {
                                say(&format!(
                                    "windsurf: {shown} already has engram — leaving it"
                                ));
                            }
                        } else {
                            v.as_object_mut()
                                .expect("checked is_object")
                                .entry("mcpServers")
                                .or_insert_with(|| serde_json::json!({}));
                            match v["mcpServers"].as_object_mut() {
                                Some(servers) => {
                                    servers.insert("engram".into(), entry);
                                    fs::write(path, serde_json::to_string_pretty(&v)? + "\n")?;
                                    say(&format!("windsurf: added engram to {shown}"));
                                }
                                None => {
                                    say(&format!(
                                        "windsurf: {shown} has a non-object mcpServers — add this to it manually:"
                                    ));
                                    println!("    {}", self.mcp_snippet(false));
                                }
                            }
                        }
                    }
                    _ => {
                        say(&format!(
                            "windsurf: {shown} isn't valid JSON — add this to its mcpServers manually:"
                        ));
                        println!("    {}", self.mcp_snippet(false));
                    }
                }
            }
            Err(_) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(
                    path,
                    serde_json::to_string_pretty(&serde_json::json!({
                        "mcpServers": { "engram": entry }
                    }))? + "\n",
                )?;
                say(&format!(
                    "windsurf: wrote {shown} (global — one db-less entry serves every project via MCP roots)"
                ));
            }
        }
        Ok(())
    }

    /// opencode.json / kilo.json share the {"mcp": {..., "type": "local"}} shape.
    pub(super) fn wire_mcp_array(&self, rel: &str, label: &str) -> anyhow::Result<()> {
        let path = self.repo.join(rel);
        if path.exists() {
            let current = fs::read_to_string(&path)?;
            if current.contains("\"engram\"") {
                if let Some(next) = self.repaired_json_config(&current) {
                    fs::write(&path, next)?;
                    say(&format!(
                        "{label}: re-pointed {rel}'s engram entry at this binary (was the pre-rename `engram`)"
                    ));
                } else {
                    say(&format!("{label}: {rel} already has engram — leaving it"));
                }
            } else {
                say(&format!(
                    "{label}: {rel} exists — add this to its \"mcp\" block manually:"
                ));
                println!(
                    "    \"engram\": {{ \"type\": \"local\", \"command\": [{}, \"mcp\", \"--db\", {}], \"enabled\": true }}",
                    json_str(&self.bin),
                    json_str(&self.db)
                );
            }
        } else {
            fs::write(
                &path,
                format!(
                    "{{\n  \"mcp\": {{\n    \"engram\": {{\n      \"type\": \"local\",\n      \"command\": [{}, \"mcp\", \"--db\", {}],\n      \"enabled\": true\n    }}\n  }}\n}}\n",
                    json_str(&self.bin),
                    json_str(&self.db)
                ),
            )?;
            say(&format!("{label}: wrote {rel}"));
        }
        self.write_instructions("AGENTS.md")
    }
}

/// Whether the Engram plugin is installed in Claude Code — any
/// `engram@<marketplace>` entry in the plugin registry
/// (`$CLAUDE_CONFIG_DIR` or `~/.claude`, `plugins/installed_plugins.json`).
pub fn claude_plugin_installed() -> bool {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".claude"))
        });
    let Some(base) = base else { return false };
    fs::read_to_string(base.join("plugins/installed_plugins.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v["plugins"].as_object().cloned())
        .is_some_and(|plugins| plugins.keys().any(|k| k.starts_with("engram@")))
}

/// Whether the repo's `.mcp.json` registers a server named `engram`.
pub fn mcp_json_has_engram(repo: &std::path::Path) -> bool {
    fs::read_to_string(repo.join(".mcp.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|v| v["mcpServers"].get("engram").is_some())
}
