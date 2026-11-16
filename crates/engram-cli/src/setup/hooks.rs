//! Session-start brief hooks for each assistant that has them.

use super::*;

impl Setup {
    /// Install the SessionStart brief hook: the script under `.claude/hooks/`
    /// plus its registration in `.claude/settings.json`. A foreign settings
    /// file is never rewritten — the snippet is printed instead (same policy
    /// as `write_mcp_servers`).
    pub(super) fn install_claude_brief_hook(&self) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(".claude/hooks");
        fs::create_dir_all(&hooks_dir)?;
        for (name, body) in [
            ("engram-brief.sh", SESSION_BRIEF_HOOK),
            ("engram-refs.sh", FILE_READ_MATCH_HOOK),
        ] {
            let script = hooks_dir.join(name);
            fs::write(&script, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
            }
        }

        let registration = r#"{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/engram-brief.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Read|Edit|Write|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/engram-refs.sh"
          }
        ]
      }
    ]
  }
}
"#;
        let settings = self.repo.join(".claude/settings.json");
        if settings.exists() {
            let current = fs::read_to_string(&settings)?;
            let has_brief = current.contains("engram-brief") || current.contains("session-brief");
            let has_refs = current.contains("engram-refs") || current.contains("file-read-match");
            if has_brief && has_refs {
                say("claude: .claude/settings.json already runs both hooks — leaving it");
            } else {
                say("claude: .claude/settings.json exists — merge the missing hook(s) from:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&settings, registration)?;
        say("claude: brief + file-read-match hooks installed (.claude/hooks + settings.json)");
        Ok(())
    }

    /// Codex SessionStart: envelope scripts under `.codex/hooks/`, registered in
    /// `.codex/hooks.json`. Codex trusts hooks by hash, so the user reviews them
    /// once with `/hooks`; a foreign hooks file gets the snippet printed instead.
    /// The command finds the repo root itself (sessions may start in a subdir).
    pub(super) fn install_codex_brief_hook(&self) -> anyhow::Result<()> {
        self.write_envelope_brief_scripts(".codex/hooks")?;

        let registration = r#"{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"/.codex/hooks/engram-brief.sh",
            "statusMessage": "Loading Engram brief"
          }
        ]
      }
    ]
  }
}
"#;
        let config = self.repo.join(".codex/hooks.json");
        if config.exists() {
            let current = fs::read_to_string(&config)?;
            if current.contains("engram-brief") {
                say("codex: .codex/hooks.json already runs the brief hook — leaving it");
            } else {
                say("codex: .codex/hooks.json exists — merge the SessionStart hook from:");
                println!("{registration}");
            }
        } else {
            fs::write(&config, registration)?;
            say("codex: brief hook installed (.codex/hooks + hooks.json)");
        }
        say(
            "codex: project hooks are trust-gated — run `/hooks` inside codex once to review and trust the engram hook (older codex builds also need `codex --enable skills` for .codex/skills)",
        );
        Ok(())
    }

    /// Cursor sessionStart: envelope scripts under `.cursor/hooks/` (flat
    /// `additional_context` output), merged into `.cursor/hooks.json`. Other hooks
    /// survive; an existing brief hook is left alone; unparseable gets the snippet.
    pub(super) fn install_cursor_brief_hook(&self) -> anyhow::Result<()> {
        self.write_envelope_brief_scripts(".cursor/hooks")?;
        let entry = serde_json::json!({ "command": "./.cursor/hooks/engram-brief.sh cursor" });
        let path = self.repo.join(".cursor/hooks.json");
        let mut doc = match fs::read_to_string(&path) {
            Err(_) => serde_json::json!({ "version": 1, "hooks": {} }),
            Ok(current) if current.contains("engram-brief") => {
                say("cursor: .cursor/hooks.json already runs the brief hook — leaving it");
                return Ok(());
            }
            Ok(current) => match serde_json::from_str::<serde_json::Value>(&current) {
                Ok(v)
                    if v.is_object() && v.get("hooks").is_none_or(serde_json::Value::is_object) =>
                {
                    v
                }
                _ => {
                    say(
                        "cursor: .cursor/hooks.json isn't a hooks object — add this under hooks.sessionStart:",
                    );
                    println!("    {entry}");
                    return Ok(());
                }
            },
        };
        let hooks = doc
            .as_object_mut()
            .expect("checked is_object")
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        let session_start = hooks
            .as_object_mut()
            .expect("checked hooks is an object")
            .entry("sessionStart")
            .or_insert_with(|| serde_json::json!([]));
        match session_start.as_array_mut() {
            Some(list) => list.push(entry),
            None => {
                say("cursor: .cursor/hooks.json has a non-array sessionStart — add this to it:");
                println!("    {entry}");
                return Ok(());
            }
        }
        if doc.get("version").is_none() {
            doc["version"] = serde_json::json!(1);
        }
        fs::write(&path, serde_json::to_string_pretty(&doc)? + "\n")?;
        say("cursor: brief hook installed (.cursor/hooks + hooks.json sessionStart)");
        Ok(())
    }

    /// The two brief-hook scripts for a JSON-envelope harness (Devin, Codex,
    /// Cursor):
    /// the shared portable brief script plus the envelope wrapper, installed
    /// under `<dir>/` as engram-brief-text.sh / engram-brief.sh.
    fn write_envelope_brief_scripts(&self, hooks_dir_rel: &str) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(hooks_dir_rel);
        fs::create_dir_all(&hooks_dir)?;
        for (name, body) in [
            ("engram-brief-text.sh", SESSION_BRIEF_HOOK),
            ("engram-brief.sh", ENVELOPE_BRIEF_HOOK),
        ] {
            let script = hooks_dir.join(name);
            fs::write(&script, body)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    }

    /// The Devin SessionStart hook: the envelope scripts under
    /// `.devin/hooks/` and their registration in `.devin/hooks.v1.json`. A
    /// foreign hooks file is never rewritten — the snippet is printed
    /// instead (same policy as Claude's settings.json).
    pub(super) fn install_devin_brief_hook(&self) -> anyhow::Result<()> {
        self.write_envelope_brief_scripts(".devin/hooks")?;

        let registration = r#"{
  "SessionStart": [
    {
      "hooks": [
        {
          "type": "command",
          "command": "./.devin/hooks/engram-brief.sh"
        }
      ]
    }
  ]
}
"#;
        let config = self.repo.join(".devin/hooks.v1.json");
        if config.exists() {
            let current = fs::read_to_string(&config)?;
            if current.contains("engram-brief") {
                say("devin: .devin/hooks.v1.json already runs the brief hook — leaving it");
            } else {
                say("devin: .devin/hooks.v1.json exists — merge the SessionStart hook from:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&config, registration)?;
        say("devin: brief hook installed (.devin/hooks + hooks.v1.json)");
        Ok(())
    }

    /// Bob SessionStart: the brief script under `.bob/hooks/`, registered in
    /// `.bob/settings.json` (foreign files get the snippet). `sh`-prefixed and
    /// workspace-relative so a lost exec bit can't break it; explicit timeout
    /// because Bob's 10 s default is tight when the CLI opens the store itself.
    pub(super) fn install_bob_brief_hook(&self) -> anyhow::Result<()> {
        let hooks_dir = self.repo.join(".bob/hooks");
        fs::create_dir_all(&hooks_dir)?;
        let script = hooks_dir.join("engram-brief.sh");
        fs::write(&script, SESSION_BRIEF_HOOK)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        }

        let registration = r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "sh .bob/hooks/engram-brief.sh",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
"#;
        let settings = self.repo.join(".bob/settings.json");
        if settings.exists() {
            let current = fs::read_to_string(&settings)?;
            if current.contains("engram-brief") {
                say("bob: .bob/settings.json already runs the brief hook — leaving it");
            } else {
                say("bob: .bob/settings.json exists — merge the SessionStart hook from:");
                println!("{registration}");
            }
            return Ok(());
        }
        fs::write(&settings, registration)?;
        say(
            "bob: brief hook installed (.bob/hooks + settings.json — needs Bob IDE 2.0.2+ / BobShell 2.0.1+)",
        );
        Ok(())
    }
}
