//! Lane-2 e2e (0.8.13): what `setup` wires is what a session actually gets —
//! the generated skills carry the current teaching, the installed hooks
//! deliver a brief on a FRESH repo (issue #8's first report), and the MCP
//! config launches a bridge that really serves the current tool surface
//! (issue #9's schema fixes, observed on the wire).

mod e2e_common;

use e2e_common::*;
use std::process::Command;

/// `setup --cli devin,codex,windsurf,bob` writes the whole adapter surface,
/// and the installed skills/instructions are the CURRENT generation — the
/// 0.8.13 keyword-JSON teaching, not a stale embed.
#[test]
fn setup_writes_wiring_and_current_generation_skills() {
    let sb = Sandbox::new("setupgen", 19400);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["setup", "--cli", "devin,codex,windsurf,bob"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup failed: {out:?}");

    // Devin: local MCP config (git-ignored), skills, hook, AGENTS.md.
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".devin/mcp_config.local.json")).unwrap(),
    )
    .unwrap();
    let engram = &config["mcpServers"]["engram"];
    assert!(engram["command"].as_str().is_some_and(|c| !c.is_empty()));
    assert_eq!(engram["args"][0], "mcp");
    assert!(
        std::fs::read_to_string(proj.join(".gitignore"))
            .unwrap_or_default()
            .contains("mcp_config.local.json"),
        "the personal config is git-ignored"
    );

    // The installed capture skill teaches the current call shapes.
    let skill = std::fs::read_to_string(proj.join(".devin/skills/engram/SKILL.md")).unwrap();
    for marker in [
        "keyword JSON",
        r#"add_notes {"notes": ["#,
        "`Decision`, `Principle`, `Caution`, `Problem`, `Resolution`, `Insight`, `Intent`, `Anchor`",
    ] {
        assert!(skill.contains(marker), "capture skill misses {marker:?}");
    }
    let digest =
        std::fs::read_to_string(proj.join(".devin/skills/engram-digest/SKILL.md")).unwrap();
    assert!(
        digest.contains("items carry no links"),
        "digest skill teaches the two-pass link rule"
    );

    // AGENTS.md — the only surface guaranteed in a Devin/Codex context.
    let agents = std::fs::read_to_string(proj.join("AGENTS.md")).unwrap();
    for marker in [
        "Anchor",
        "describe_ontology",
        r#"add_note {"type": "Decision""#,
        "returned ids in a second pass",
    ] {
        assert!(agents.contains(marker), "AGENTS.md misses {marker:?}");
    }

    // Hooks: devin + codex share the envelope wrapper; bob runs the portable
    // script directly (plain-stdout SessionStart, Bob IDE 2.0.2+). All three
    // registrations point at their script.
    for (script, registration) in [
        (".devin/hooks/engram-brief.sh", ".devin/hooks.v1.json"),
        (".codex/hooks/engram-brief.sh", ".codex/hooks.json"),
        (".bob/hooks/engram-brief.sh", ".bob/settings.json"),
    ] {
        assert!(proj.join(script).exists(), "{script} missing");
        assert!(
            std::fs::read_to_string(proj.join(registration))
                .unwrap()
                .contains("engram-brief"),
            "{registration} doesn't run the hook"
        );
    }
    assert!(proj.join(".codex/skills/engram/SKILL.md").exists());

    // Bob: the registration is valid Claude-Code-shaped JSON (event → group
    // → command hooks), and the MCP config keeps its explicit --db.
    let bob: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join(".bob/settings.json")).unwrap())
            .unwrap();
    assert!(
        bob["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .is_some_and(|c| c.contains("engram-brief.sh")),
        "bob settings.json misses the SessionStart command"
    );
    let bob_mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join(".bob/mcp.json")).unwrap())
            .unwrap();
    assert!(
        bob_mcp["mcpServers"]["engram"]["args"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "--db")),
        "bob mcp.json keeps the explicit --db"
    );

    // Windsurf: the always_on rule (no hooks there) + skills.
    let rule = std::fs::read_to_string(proj.join(".windsurf/rules/engram.md")).unwrap();
    let body = rule
        .splitn(3, "---")
        .nth(2)
        .expect("the rule has frontmatter");
    assert!(
        body.lines()
            .find(|l| !l.trim().is_empty())
            .is_some_and(|l| l.contains("brief")),
        "the rule leads with the brief-rebind instruction: {rule}"
    );
    assert!(proj.join(".windsurf/skills/engram/SKILL.md").exists());
}

/// Issue #8's first report, as a test: on a FRESH repo that `serve` just
/// wired, the installed SessionStart hook must deliver a brief — inside the
/// JSON envelope Devin/Codex require, with the store materialized on disk.
#[test]
fn devin_brief_hook_delivers_envelope_on_fresh_repo() {
    let sb = Sandbox::new("hookbrief", 19420);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["setup", "--cli", "devin"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup failed: {out:?}");
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "serve failed: {out:?}");
    sb.wait_core_healthy(CORE_HEALTH_WINDOW);

    // Run the hook exactly as the harness would: cwd = repo, no extra args,
    // the same env the user's shell gives the sandbox.
    let out = Command::new("bash")
        .arg(proj.join(".devin/hooks/engram-brief.sh"))
        .current_dir(&proj)
        .env("ENGRAM_HOME", &sb.home)
        .env("HOME", sb.root.join("home"))
        .output()
        .unwrap();
    assert!(out.status.success(), "hook failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook output is not the JSON envelope ({e}): {stdout}"));
    assert_eq!(
        envelope["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let context = envelope["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("envelope carries context");
    assert!(
        context.contains("Engram brief"),
        "the context is the brief: {context}"
    );
    // A fresh graph's brief is a cold start — which since 0.8.13 teaches the
    // ontology (the one channel reaching a model with no skill loaded).
    assert!(
        context.contains("cold start") && context.contains("Anchor"),
        "the cold-start brief teaches the ontology: {context}"
    );
}

/// The wiring is executable: launching EXACTLY what the Devin MCP config
/// says (command + args, verbatim) yields a bridge serving the current tool
/// surface — 26 tools, no retired set_project, and no $ref-hidden schemas
/// (issue #9's wire contract, observed end to end).
#[test]
fn devin_mcp_config_launches_a_bridge_serving_current_tools() {
    let sb = Sandbox::new("cfgbridge", 19440);
    let proj = sb.project("alpha");

    let out = sb
        .cmd(&["setup", "--cli", "devin"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup failed: {out:?}");
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "serve failed: {out:?}");
    sb.wait_core_healthy(CORE_HEALTH_WINDOW);

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".devin/mcp_config.local.json")).unwrap(),
    )
    .unwrap();
    let engram = &config["mcpServers"]["engram"];
    let mut cmd = Command::new(engram["command"].as_str().unwrap());
    for arg in engram["args"].as_array().unwrap() {
        cmd.arg(arg.as_str().unwrap());
    }
    // The sandbox env the harness would inherit from the user's shell.
    cmd.current_dir(&proj)
        .env("ENGRAM_HOME", &sb.home)
        .env("HOME", sb.root.join("home"))
        .env("ENGRAM_HTTP_PORT", sb.port.to_string())
        .env("ENGRAM_UPDATE_CHECK", "0");

    let mut bridge = Bridge::spawn_cmd(cmd);
    bridge.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let tools = bridge.recv();
    for present in ["add_note", "add_notes", "brief", "search", "link"] {
        assert!(tools.contains(present), "tools/list misses {present}");
    }
    assert!(
        !tools.contains("set_project"),
        "the retired set_project must not be served"
    );
    assert!(
        !tools.contains("$ref"),
        "no schema hides fields behind a $ref on the wire"
    );
    bridge.kill();
}

/// The Claude Code plugin's server (0.9.9) is `mcp --wired-only`, started in
/// every folder Claude Code opens: a folder that isn't an Engram project is
/// never turned into one (reads the home graph, project writes refused with
/// the wiring instruction), and a folder anywhere inside a wired project
/// binds that project — without growing a stray `.engram/` of its own.
#[test]
fn wired_only_bridge_never_creates_a_project() {
    let sb = Sandbox::new("wiredonly", 19460);

    let loose = sb.root.join("loose-repo");
    std::fs::create_dir_all(loose.join(".git")).unwrap();
    let mut bridge =
        Bridge::spawn_cmd(sb.cmd(&["mcp", "--wired-only", "--fake-embeddings"], &loose));
    let brief = bridge.call(2, "brief", "{}");
    assert!(brief.contains("not an Engram project yet"), "{brief}");
    let write = bridge.call(
        3,
        "add_note",
        r#"{"type":"Insight","title":"a stray capture","body":"x"}"#,
    );
    assert!(
        write.contains("write refused") && write.contains("/engram:setup"),
        "{write}"
    );
    // A deliberate user-level write still works.
    let home = bridge.call(
        4,
        "add_note",
        r#"{"type":"Insight","title":"a deliberate user-level note","body":"x","project":"home"}"#,
    );
    assert!(home.contains("created"), "{home}");
    bridge.kill();
    assert!(
        !loose.join(".engram").exists(),
        "the loose repo stayed unwired"
    );

    let wired = sb.project("wired");
    let deep = wired.join("src/deep");
    std::fs::create_dir_all(&deep).unwrap();
    // Registered the way `serve`/setup leave it: the project's own bridge
    // binds once, which registers the root.
    let mut own = Bridge::spawn_cmd(sb.cmd(&["mcp", "--fake-embeddings"], &wired));
    let bound = own.call(2, "brief", "{}");
    assert!(bound.contains("project 'wired'"), "{bound}");
    own.kill();
    let mut inner = Bridge::spawn_cmd(sb.cmd(&["mcp", "--wired-only", "--fake-embeddings"], &deep));
    let write = inner.call(
        2,
        "add_note",
        r#"{"type":"Insight","title":"captured from a subfolder","body":"x"}"#,
    );
    assert!(
        write.contains("created") && write.contains("wired"),
        "{write}"
    );
    assert!(
        !deep.join(".engram").exists(),
        "no stray .engram in the subfolder"
    );
}

/// With the Engram Claude Code plugin installed, `setup --cli claude` wires
/// the repo (git-ignore + `.engram/`) but writes no project `.mcp.json`
/// entry — the plugin already serves MCP, and a second entry would load
/// every engram tool twice.
#[test]
fn setup_defers_mcp_to_the_claude_plugin() {
    let sb = Sandbox::new("claudeplugin", 19480);
    let repo = sb.root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let plugins = sb.root.join("home/.claude/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        plugins.join("installed_plugins.json"),
        r#"{"version":2,"plugins":{"engram@engram":[{"scope":"user","version":"0.9.9"}]}}"#,
    )
    .unwrap();

    let out = sb
        .cmd(&["setup", "--cli", "claude", "--mcp-only"], &repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup failed: {out:?}");
    let said =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("plugin"), "{said}");
    assert!(!repo.join(".mcp.json").exists(), "no duplicate MCP entry");
    assert!(repo.join(".engram").is_dir(), "the repo is marked wired");
    assert!(
        std::fs::read_to_string(repo.join(".gitignore"))
            .unwrap()
            .contains(".engram/")
    );

    // Without the plugin, setup writes the project entry as before.
    std::fs::remove_file(plugins.join("installed_plugins.json")).unwrap();
    let out = sb
        .cmd(&["setup", "--cli", "claude", "--mcp-only"], &repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(repo.join(".mcp.json").exists());
}

/// 0.11.1 (issue #19): a bridge binding a project is what brings its
/// installed instructions up to the core's binary. The core rewrites the
/// copy git doesn't track, holds the tracked one (the brief and the setup
/// record say so), and `setup --refresh` updates it on purpose.
#[test]
fn a_bridge_binding_makes_the_core_keep_the_project_up() {
    let sb = Sandbox::new("upkeep", 19680);
    let proj = sb.project("alpha");
    let out = sb
        .cmd(&["setup", "--cli", "devin", "--skill", "normal"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup failed: {out:?}");
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join(".engram/setup.json")).unwrap())
            .unwrap();
    assert_eq!(record["setup_version"], REAL_VERSION);
    assert_eq!(record["agents"], serde_json::json!(["devin"]));
    assert_eq!(record["variant"], "normal");

    // Age both copies to an older release; commit only AGENTS.md.
    let skill = proj.join(".devin/skills/engram/SKILL.md");
    let agents = proj.join("AGENTS.md");
    for f in [&skill, &agents] {
        let text = std::fs::read_to_string(f).unwrap();
        let aged = text.replace(&format!("v{REAL_VERSION} "), "v0.10.2 ");
        assert_ne!(text, aged, "{} carries a stamp", f.display());
        std::fs::write(f, aged).unwrap();
    }
    let aged_agents = std::fs::read_to_string(&agents).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&proj)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "AGENTS.md"]);
    git(&["commit", "-q", "-m", "instructions"]);

    // status --setup reads the registry without a core (issue #19).
    let status = |args: &[&str]| {
        let out = sb.cmd(args, &proj).output().unwrap();
        assert!(out.status.success(), "{args:?} failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let before = status(&["status", "--setup"]);
    assert!(
        before.contains("alpha (")
            && before.contains(&format!("set up by v{REAL_VERSION} · devin · normal"))
            && before.contains("1 skill copy — the core updates")
            && before.contains("1 outdated copy tracked by git (AGENTS.md)"),
        "{before}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&status(&["status", "--setup", "--json"])).unwrap();
    let setup = &json["projects"][0]["setup"];
    assert_eq!(setup["wiring"], "current");
    assert_eq!(setup["held"], serde_json::json!(["AGENTS.md"]));
    assert_eq!(setup["current"], false);

    let mut bridge = Bridge::spawn(&sb, &proj);
    let current = format!("engram-skill engram v{REAL_VERSION} normal");
    assert!(
        eventually(CORE_HEALTH_WINDOW, || std::fs::read_to_string(&skill)
            .is_ok_and(|s| s.contains(&current))),
        "the core never refreshed the untracked skill"
    );
    assert!(
        eventually(std::time::Duration::from_secs(10), || {
            std::fs::read_to_string(proj.join(".engram/setup.json"))
                .is_ok_and(|r| r.contains("\"held\"") && r.contains("AGENTS.md"))
        }),
        "the tracked AGENTS.md is recorded as held"
    );
    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        aged_agents,
        "the tracked copy is left byte for byte"
    );
    let brief = bridge.call(2, "brief", "{}");
    assert!(
        brief.contains("AGENTS.md (0.10.2)") && brief.contains("setup --refresh"),
        "the brief flags the held copy: {brief}"
    );

    let out = sb.cmd(&["setup", "--refresh"], &proj).output().unwrap();
    assert!(out.status.success(), "setup --refresh failed: {out:?}");
    assert!(
        std::fs::read_to_string(&agents)
            .unwrap()
            .contains(&format!("engram:begin v{REAL_VERSION} normal"))
    );
    let record = std::fs::read_to_string(proj.join(".engram/setup.json")).unwrap();
    assert!(!record.contains("\"held\""), "{record}");
    let brief = bridge.call(3, "brief", "{}");
    assert!(!brief.contains("setup --refresh"), "{brief}");
    let after = status(&["status", "--setup"]);
    assert!(
        after.contains("project setup: 1 project current"),
        "{after}"
    );
    // The full status carries the same line under the project.
    let full = status(&["status"]);
    assert!(
        full.contains(&format!(
            "set up by v{REAL_VERSION} · devin · normal · current"
        )),
        "{full}"
    );
}
