use std::{
    io::{self, BufRead, Read, Write},
    panic,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};
use tinkr_core::{
    auto::{self, HealthState},
    filters::{self, Ctx, Decision},
    global_scope,
    hook::{self, Agent, Permissions},
    ledger::{Event, Layer, Ledger},
    manifest::ChangeSet,
    mcp, overlap,
    platform::{self, Native, Platform},
    profile::{self, Action, ActionResult, Machine},
    provider::Provider,
    recall::RecallStore,
    remote, runner, scope,
    tokenizer::TokenizerKind,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tinkr: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let n = Native;
    let paths = platform::paths(&n)?;
    let caps = platform::terminal(&n);

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("tinkr {VERSION}");
        }
        Some("--help" | "-h" | "help" | "/?") => {
            print_help();
        }
        None => {
            cmd_default(&n, &paths, args)?;
        }
        Some("setup") => {
            let yes = args.iter().any(|a| a == "--yes" || a == "-y");
            let all = args.iter().any(|a| a == "--all");
            cmd_setup(&n, &paths, yes, all)?;
        }
        Some("gain") => {
            let s = Ledger::open(&paths.ledger_db())?.summary(None)?;
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "{} events, {} -> {} tokens, saved {} ({:.1}%) [{} measured, {} estimated]",
                    s.events,
                    s.before,
                    s.after,
                    s.saved,
                    s.percent,
                    s.measured_events,
                    s.estimated_events
                );
                if !s.by_layer.is_empty() {
                    println!("By layer:");
                    for (name, l) in &s.by_layer {
                        println!(
                            "  {name:<12} {} -> {} tokens (saved {})",
                            l.before, l.after, l.saved
                        );
                    }
                }
                if !s.by_provider.is_empty() {
                    println!("By provider:");
                    for (name, l) in &s.by_provider {
                        println!(
                            "  {name:<18} {} -> {} tokens (saved {})",
                            l.before, l.after, l.saved
                        );
                    }
                }
                if !s.by_tool.is_empty() {
                    println!("By tool:");
                    for (name, l) in &s.by_tool {
                        println!(
                            "  {name:<18} {} -> {} tokens (saved {})",
                            l.before, l.after, l.saved
                        );
                    }
                }
            }
        }
        Some("recall") => {
            let id = args.get(1).ok_or("usage: tinkr recall <id>")?;
            let data = RecallStore::open(&paths.recall_dir())?.get(id)?;
            if let Ok(ledger) = Ledger::open(&paths.ledger_db()) {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let _ = ledger.record_recall(id, ts);
            }
            io::stdout().write_all(&data)?;
        }
        Some("undo") => {
            let home = n.home_dir()?;
            let store = n.env_store(&home, &paths);
            let mut cs = ChangeSet::open(&paths.manifest())?;
            let r = cs.undo(store.as_ref())?;
            println!(
                "restored {} file(s), removed {}, reset {} env var(s)",
                r.restored.len(),
                r.removed.len(),
                r.env_restored.len()
            );
            for s in &r.skipped {
                println!("note: {s}");
            }
            for e in &r.errors {
                eprintln!("error: {e}");
            }
            if args.iter().any(|a| a == "--purge") && r.errors.is_empty() {
                for d in [&paths.data, &paths.cache, &paths.config] {
                    let _ = std::fs::remove_dir_all(d);
                }
                println!("purged Tinkr data");
            }
            if !r.errors.is_empty() {
                return Err("some changes could not be undone; re-run `tinkr undo`".into());
            }
        }
        Some("pause") => {
            let here = args.iter().any(|a| a == "--here");
            let file = if here {
                scope::project_pause_file(&std::env::current_dir()?)
            } else {
                scope::global_pause_file(&paths.config)
            };
            scope::set_paused(&file, true)?;
            println!(
                "tinkr paused ({})",
                if here { "this project" } else { "globally" }
            );
        }
        Some("resume") => {
            let here = args.iter().any(|a| a == "--here");
            let file = if here {
                scope::project_pause_file(&std::env::current_dir()?)
            } else {
                scope::global_pause_file(&paths.config)
            };
            scope::set_paused(&file, false)?;
            println!(
                "tinkr resumed ({})",
                if here { "this project" } else { "globally" }
            );
        }
        Some("doctor") => {
            let fix = args.iter().any(|a| a == "--fix");
            let net = args.iter().any(|a| a == "--network");
            cmd_doctor(&n, &paths, fix, net)?;
        }
        Some("mcp") => {
            let store = RecallStore::open(&paths.recall_dir())?;
            let ledger = Ledger::open(&paths.ledger_db()).ok();
            let stdin = io::stdin();
            let stdout = io::stdout();
            mcp::serve_stdio_with_ledger(
                stdin.lock(),
                stdout.lock(),
                &store,
                ledger.as_ref(),
                caps.unicode,
            )?;
        }
        Some("hook") => {
            let agent = args.get(1).and_then(|s| Agent::parse(s));
            if let Some(agent) = agent {
                let cwd = std::env::current_dir().unwrap_or_default();
                if !scope::is_paused(&paths.config, &cwd) {
                    let mut buf = String::new();
                    if io::stdin().read_to_string(&mut buf).is_ok() {
                        if agent == Agent::Antigravity {
                            if let Some(ev) = hook::extract_antigravity_stop(&buf) {
                                if let Ok(ledger) = Ledger::open(&paths.ledger_db()) {
                                    if !ledger
                                        .has_session_event(Layer::SayLess, &ev.session_key)
                                        .unwrap_or(false)
                                    {
                                        let ts = SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs() as i64;
                                        let _ = ledger.record(&Event {
                                            ts,
                                            layer: Layer::SayLess,
                                            provider: ev.provider,
                                            tool: "antigravity".to_string(),
                                            session: Some(ev.session_key),
                                            before: ev.before,
                                            after: ev.after,
                                            kind: tinkr_core::ledger::CountKind::Estimated,
                                            recall_id: None,
                                        });
                                    }
                                }
                                println!("{{\"decision\":\"allow\"}}");
                                return Ok(ExitCode::SUCCESS);
                            }
                        }
                        let exe = std::env::current_exe()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| "tinkr".to_string());
                        let mut perms = Permissions::default();
                        if agent == Agent::Claude {
                            if let Ok(home) = n.home_dir() {
                                let m = Machine::real(home);
                                if let Some(p) = profile::builtin()
                                    .into_iter()
                                    .find(|x| x.id == "claude-code")
                                {
                                    if let Some(dir) = m.config_dir(&p) {
                                        if let Ok(t) =
                                            std::fs::read_to_string(dir.join("settings.json"))
                                        {
                                            perms.merge_from(&t);
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(out) = hook::handle(agent, &buf, &exe, &perms) {
                            println!("{out}");
                        }
                    }
                }
            }
        }
        Some("run") => {
            return cmd_run(&paths, &args[1..], caps.unicode);
        }
        Some("overlap") => {
            cmd_overlap(&n, &paths, &args[1..])?;
        }
        Some(other) => return Err(format!("unknown command '{other}' (try `tinkr --help`)").into()),
    }
    Ok(ExitCode::SUCCESS)
}

fn print_help() {
    println!(
        "tinkr {VERSION} - Tinkr optimizes your agent's context so you don't have to.\n\n\
         Commands:\n  \
           tinkr                    First run = auto-setup; afterwards = status\n  \
           tinkr gain [--json]      Show token savings by layer, provider, and tool\n  \
           tinkr doctor [--fix]     Diagnose OS, shells, agents, coverage, and repair drift\n  \
           tinkr recall <id>        Fetch original uncompressed output by recall id\n  \
           tinkr undo [--purge]     Restore all edited configs and env vars byte-for-byte\n  \
           tinkr pause [--here]     Pause Tinkr globally or for the current project\n  \
           tinkr resume [--here]    Resume Tinkr\n  \
           tinkr run -- <cmd>       Run a shell command through the read-less filter\n  \
           tinkr overlap            Detect competing token optimizers and manage coexistence\n  \
           tinkr mcp                Start the stdio MCP server (tinkr_retrieve/compress/read)"
    );
}

fn cmd_overlap(
    n: &Native,
    paths: &platform::Paths,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let home = n.home_dir()?;
    let state_path = paths.data.join("overlap-state.json");
    let mut state = overlap::OverlapState::load(&state_path);

    let loaded = overlap::load_builtin("2026-11-24");
    let env = overlap::ScanEnv::real(home.clone(), None);
    let scan_report = overlap::scan(&loaded.profiles, &env);
    let m = Machine::real(home);
    let detected_agents: Vec<String> = profile::builtin()
        .into_iter()
        .filter(|p| profile::detect(p, &m).is_some())
        .map(|p| p.id)
        .collect();

    if args.first().map(String::as_str) == Some("restore") {
        let tool = args.get(1).ok_or("usage: tinkr overlap restore <tool>")?;
        let mut cs = ChangeSet::open(&paths.manifest())?;
        let rep = overlap::restore_tool(tool, &mut cs, &mut state)?;
        state.save(&state_path)?;
        println!(
            "Restored {} file(s) for '{}' (errors: {})",
            rep.restored.len(),
            tool,
            rep.errors.len()
        );
        for e in &rep.errors {
            eprintln!("  error: {e}");
        }
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("choose") {
        let tool = args
            .get(1)
            .ok_or("usage: tinkr overlap choose <tool> keep|tinkr|both|compare [--agent X]")?;
        let choice_s = args
            .get(2)
            .ok_or("usage: tinkr overlap choose <tool> keep|tinkr|both|compare [--agent X]")?;
        let choice = overlap::UserChoice::parse(choice_s)
            .ok_or("choice must be one of: keep, tinkr, both, compare")?;
        let agent = args
            .windows(2)
            .find(|w| w[0] == "--agent")
            .map(|w| w[1].as_str());
        let pre = overlap::analyze(
            &scan_report,
            &loaded.profiles,
            &detected_agents,
            &state.global_choices(),
        );
        let (snap_agents, snap_layers) = pre
            .tools
            .iter()
            .find(|t| t.id == *tool)
            .map(|t| (t.wired_agents.clone(), t.active_layers.clone()))
            .unwrap_or_default();
        state.set_choice(tool, agent, choice, snap_agents, snap_layers);
        if choice == overlap::UserChoice::Tinkr {
            if let Some(prof) = loaded.profiles.iter().find(|p| p.id == *tool) {
                let plan = overlap::plan_disable_overlapping(prof, &m.home, agent);
                if !plan.edits.is_empty() {
                    println!(
                        "Planned reversible edits to disable overlapping {} hooks:",
                        prof.name
                    );
                    for e in &plan.edits {
                        print!("{}", e.diff_preview);
                    }
                    let confirmed = args.iter().any(|a| a == "--confirm") || {
                        print!("Apply these reversible edits? [y/N]: ");
                        io::stdout().flush()?;
                        let mut ans = String::new();
                        io::stdin().lock().read_line(&mut ans)?;
                        matches!(ans.trim().to_ascii_lowercase().as_str(), "y" | "yes")
                    };
                    if confirmed {
                        let mut cs = ChangeSet::open(&paths.manifest())?;
                        match overlap::apply_disable_plan(&plan, &mut cs, &mut state, false)? {
                            overlap::SwitchOutcome::Applied {
                                files_modified,
                                manual_hints,
                            } => {
                                println!(
                                    "Disabled overlapping entries in {} file(s).",
                                    files_modified.len()
                                );
                                for h in manual_hints {
                                    println!("  note: {h}");
                                }
                            }
                            overlap::SwitchOutcome::EditWarStopped { tool_id, repairs } => {
                                println!(
                                    "Stopped repairing '{tool_id}' after {repairs} re-wirings to avoid an edit war."
                                );
                            }
                            overlap::SwitchOutcome::NothingToEdit { manual_hints } => {
                                for h in manual_hints {
                                    println!("  manual step: {h}");
                                }
                            }
                        }
                    } else {
                        println!("No files modified. Keeping existing tool active.");
                        return Ok(());
                    }
                } else {
                    for h in &plan.manual_hints {
                        println!("  manual off-switch: {h}");
                    }
                }
            }
        }
        state.save(&state_path)?;
        println!(
            "Saved overlap choice: {} -> {}{}",
            tool,
            choice.as_str(),
            agent.map(|a| format!(" (agent: {a})")).unwrap_or_default()
        );
        return Ok(());
    }

    let analysis = overlap::analyze(
        &scan_report,
        &loaded.profiles,
        &detected_agents,
        &state.global_choices(),
    );

    let json_out = args.iter().any(|a| a == "--json");
    if json_out {
        println!("{}", serde_json::to_string_pretty(&analysis)?);
    } else {
        print!("{}", overlap::format_plain(&analysis, true));
    }
    Ok(())
}

fn cmd_default(
    n: &Native,
    paths: &platform::Paths,
    _args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let cs = ChangeSet::open(&paths.manifest())?;
    if cs.is_empty() {
        return cmd_setup(n, paths, false, false);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let paused = scope::is_paused(&paths.config, &cwd);
    let s = Ledger::open(&paths.ledger_db())?.summary(None)?;
    println!(
        "tinkr {VERSION} ({}) — {}",
        n.name(),
        if paused { "paused" } else { "active" }
    );
    println!(
        "managed changes: {} (run `tinkr undo` to restore)",
        cs.manifest.changes.len()
    );
    println!(
        "savings: {} tokens ({:.1}% across {} events)",
        s.saved, s.percent, s.events
    );
    println!("data: {}", paths.data.display());
    Ok(())
}

fn cmd_setup(
    n: &Native,
    paths: &platform::Paths,
    auto_yes: bool,
    force_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = n.home_dir()?;
    let state_path = paths.data.join("overlap-state.json");
    let mut overlap_state = overlap::OverlapState::load(&state_path);
    let loaded_overlap = overlap::load_builtin("2026-11-24");
    let scan_env = overlap::ScanEnv::real(home.clone(), None);
    let scan_report = overlap::scan(&loaded_overlap.profiles, &scan_env);

    let m = Machine::real(home);
    let profiles = profile::builtin();
    let detected_ids: Vec<String> = profiles
        .iter()
        .filter(|p| profile::detect(p, &m).is_some())
        .map(|p| p.id.clone())
        .collect();
    let overlap_analysis = overlap::analyze(
        &scan_report,
        &loaded_overlap.profiles,
        &detected_ids,
        &overlap_state.global_choices(),
    );

    if overlap_analysis.has_active_overlap && overlap_state.needs_reprompt(&overlap_analysis) {
        if auto_yes {
            println!(
                "Overlap detected: auto-yielding overlapping layers to existing tools (run `tinkr overlap` to review)."
            );
        } else {
            print!("\n{}", overlap::format_plain(&overlap_analysis, true));
            print!("\nChoice [1/2/3/4, default 1]: ");
            io::stdout().flush()?;
            let mut ans = String::new();
            let _ = io::stdin().lock().read_line(&mut ans);
            let picked = overlap::UserChoice::parse(&ans).unwrap_or(overlap::UserChoice::Keep);
            for t in &overlap_analysis.tools {
                if matches!(
                    t.state,
                    overlap::ToolState::Active | overlap::ToolState::PartialDrifted
                ) {
                    overlap_state.set_choice(
                        &t.id,
                        None,
                        picked,
                        t.wired_agents.clone(),
                        t.active_layers.clone(),
                    );
                }
            }
            let _ = overlap_state.save(&state_path);
        }
    }

    let mut found = Vec::new();
    let mut planned = Vec::new();
    for p in &profiles {
        if let Some(reason) = profile::detect(p, &m) {
            let mut items = Vec::new();
            for a in &p.actions {
                if overlap_state.should_yield(&overlap_analysis, &p.id, overlap::action_layer(a)) {
                    continue;
                }
                if let ActionResult::Change(pl) = profile::compute(p, a, &m) {
                    items.push(pl);
                }
            }
            found.push((p, reason));
            if !items.is_empty() {
                planned.push((p, items));
            }
        }
    }

    println!("Tinkr {VERSION} — auto-setup ({})", n.name());
    if found.is_empty() {
        println!(
            "No supported AI coding tools detected yet ({} profiles available).",
            profiles.len()
        );
    } else {
        println!(
            "Detected {} of {} supported tool(s):",
            found.len(),
            profiles.len()
        );
        for (p, why) in &found {
            println!("  - {} ({why})", p.name);
        }
    }
    if !planned.is_empty() {
        println!("\nPlanned changes for detected tools (all backed up to *.tinkr.bak; `tinkr undo` restores byte-for-byte):");
        for (p, items) in &planned {
            for pl in items {
                println!("  [{}] {} ({})", p.id, pl.description, pl.file.display());
            }
        }
    } else if !found.is_empty() && !force_all {
        println!("All detected tools are already configured.");
    }

    let apply_all = if force_all {
        true
    } else if auto_yes {
        false
    } else {
        print!("\nApply changes? [y]es (auto-detected) / [a]ll (every supported tool) / [n]o: ");
        io::stdout().flush()?;
        let mut ans = String::new();
        io::stdin().lock().read_line(&mut ans)?;
        match ans.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => false,
            "a" | "all" => true,
            _ => {
                println!("Aborted; nothing changed.");
                return Ok(());
            }
        }
    };

    let targets: Vec<&profile::Profile> = if apply_all {
        profiles.iter().collect()
    } else {
        planned.iter().map(|(p, _)| *p).collect()
    };

    let mut cs = ChangeSet::open(&paths.manifest())?;
    let mut applied = 0usize;
    for p in targets {
        for r in overlap::apply_with_yield(p, &m, &mut cs, &overlap_analysis, &overlap_state)? {
            match r {
                ActionResult::Change(_) => applied += 1,
                ActionResult::Skipped { file, reason } => {
                    eprintln!("skipped {}: {reason}", file.display())
                }
                ActionResult::AlreadyDone(_) => {}
            }
        }
    }
    let home = n.home_dir()?;
    let env_store = n.env_store(&home, paths);
    let bin_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| home.join(".local").join("bin"));
    applied += global_scope::apply_global(&home, &bin_dir, env_store.as_ref(), &mut cs)?;
    println!(
        "Done ({}). Applied {applied} change(s). Run `tinkr gain` to see savings or `tinkr undo` to restore.",
        if apply_all { "all supported tools" } else { "auto-detected tools" }
    );
    Ok(())
}

fn cmd_doctor(
    n: &Native,
    paths: &platform::Paths,
    fix: bool,
    net: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = n.home_dir()?;
    let m = Machine::real(home);
    let cwd = std::env::current_dir().unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_default();
    let health_path = paths.data.join("health.json");
    let mut health = HealthState::load(&health_path);

    println!("tinkr doctor ({VERSION}, os={})", n.name());
    println!(
        "  status:  {}",
        if scope::is_paused(&paths.config, &cwd) {
            "paused"
        } else {
            "active"
        }
    );
    println!("  data:    {}", paths.data.display());
    println!("  shell:   {}", runner::default_shell());
    println!("  upgrade: {}", auto::upgrade_hint(&exe));
    let remotes = remote::detect_current();
    if !remotes.is_empty() {
        let labels: Vec<String> = remotes.iter().map(|r| r.label()).collect();
        println!("  remote:  {}", labels.join(", "));
    }
    if !health.disabled.is_empty() {
        println!("  circuit-breakers tripped: {}", health.disabled.len());
        if fix {
            health.reset_all(&health_path)?;
            println!("  -> reset all circuit-breakers");
        }
    }
    if net {
        println!("  proxy:   loopback-only, OS trust store, no TLS MITM");
        println!(
            "  HTTPS_PROXY: {}",
            std::env::var("HTTPS_PROXY").unwrap_or_else(|_| "(unset)".into())
        );
        println!(
            "  TINKR_CA_BUNDLE: {}",
            std::env::var("TINKR_CA_BUNDLE").unwrap_or_else(|_| "(unset)".into())
        );
    }

    let overlap_state = overlap::OverlapState::load(&paths.data.join("overlap-state.json"));
    let loaded_overlap = overlap::load_builtin("2026-11-24");
    let scan_report = overlap::scan(
        &loaded_overlap.profiles,
        &overlap::ScanEnv::real(m.home.clone(), None),
    );
    let detected_ids: Vec<String> = profile::builtin()
        .into_iter()
        .filter(|p| profile::detect(p, &m).is_some())
        .map(|p| p.id)
        .collect();
    let overlap_analysis = overlap::analyze(
        &scan_report,
        &loaded_overlap.profiles,
        &detected_ids,
        &overlap_state.global_choices(),
    );

    println!("\nCoverage (tool x layers):");
    println!(
        "  {:<18} {:<10} {:<10} {:<10} {:<10}",
        "TOOL", "DETECTED", "A:HOOK", "B:MCP", "D:RULES"
    );
    let mut cs = ChangeSet::open(&paths.manifest())?;
    for p in profile::builtin() {
        let det = profile::detect(&p, &m).is_some();
        let has_a = p
            .actions
            .iter()
            .any(|a| matches!(a, Action::JsonHook { .. }));
        let has_b = p
            .actions
            .iter()
            .any(|a| matches!(a, Action::McpServer { .. }));
        let has_d = p
            .actions
            .iter()
            .any(|a| matches!(a, Action::RulesBlock { .. }));
        let mark = |b: bool| if b { "yes" } else { "-" };
        println!(
            "  {:<18} {:<10} {:<10} {:<10} {:<10}",
            p.id,
            if det { "yes" } else { "no" },
            mark(has_a),
            mark(has_b),
            mark(has_d)
        );
        if det && fix {
            let _ = overlap::apply_with_yield(&p, &m, &mut cs, &overlap_analysis, &overlap_state);
        }
    }

    println!("\nOverlap & Coexistence:");
    if overlap_analysis.tools.is_empty() {
        println!("  none detected");
    } else {
        for t in &overlap_analysis.tools {
            let choice = t
                .current_choice
                .map(|c| c.as_str())
                .unwrap_or("keep (auto-yield)");
            let agents = if t.wired_agents.is_empty() {
                "-".to_string()
            } else {
                t.wired_agents.join(",")
            };
            println!(
                "  {:<24} state={:<18} agents={:<24} choice={choice}",
                t.id,
                t.state.label(),
                agents
            );
        }
    }
    for w in &loaded_overlap.warnings {
        println!("  profile warning: {w}");
    }
    for (id, rec) in &overlap_state.choices {
        if rec.repair_count >= overlap::MAX_REPAIRS {
            println!(
                "  notice: stopped repairing '{id}' after {} external re-wirings (run `tinkr overlap`)",
                rec.repair_count
            );
        }
    }
    Ok(())
}

fn cmd_run(
    paths: &platform::Paths,
    args: &[String],
    unicode: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut shell = runner::default_shell().to_string();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--shell" && i + 1 < args.len() {
            shell = args[i + 1].clone();
            i += 2;
        } else if args[i] == "--" {
            i += 1;
            break;
        } else {
            break;
        }
    }
    if i >= args.len() {
        return Err("usage: tinkr run [--shell <sh>] -- <command>".into());
    }
    let cmd = args[i..].join(" ");
    let (code, raw) = runner::run(&shell, &cmd)?;

    let cwd = std::env::current_dir().unwrap_or_default();
    if scope::is_paused(&paths.config, &cwd) || scope::is_ignored(&cwd, &cmd) {
        io::stdout().write_all(&raw)?;
        return Ok(ExitCode::from(code as u8));
    }

    let health_path = paths.data.join("health.json");
    let mut health = HealthState::load(&health_path);
    let kind_name = hook::kind_of(&cmd).name();
    let breaker_key = format!("read_less:{kind_name}");
    if health.is_disabled(&breaker_key)
        || Ledger::open(&paths.ledger_db())
            .ok()
            .is_some_and(|l| auto::should_backoff(&l, kind_name))
    {
        io::stdout().write_all(&raw)?;
        return Ok(ExitCode::from(code as u8));
    }

    let recall_dir = paths.recall_dir();
    let outcome = panic::catch_unwind(|| {
        let store = RecallStore::open(&recall_dir).ok();
        let ctx = Ctx {
            recall: store.as_ref(),
            unicode,
            tokenizer: TokenizerKind::Exact,
        };
        filters::process(&cmd, code, &raw, &ctx)
    });

    match outcome {
        Ok(out) => {
            io::stdout().write_all(&out.output)?;
            if matches!(out.decision, Decision::Compressed { .. }) && out.before > out.after {
                if let Ok(ledger) = Ledger::open(&paths.ledger_db()) {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    let _ = ledger.record(&Event {
                        ts,
                        layer: Layer::ReadLess,
                        provider: Provider::None,
                        tool: out.kind.name().to_string(),
                        session: None,
                        before: out.before,
                        after: out.after,
                        kind: out.kind_count,
                        recall_id: out.recall_id,
                    });
                }
            }
        }
        Err(_) => {
            health.trip(&breaker_key, "filter panic", &health_path);
            io::stdout().write_all(&raw)?;
        }
    }
    Ok(ExitCode::from(code as u8))
}
