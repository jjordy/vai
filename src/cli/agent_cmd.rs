//! Agent command handlers.

use crate::agent;
use super::{CliError, AgentCommands};
use super::agent_loop;

/// Handle all `vai agent` subcommands.
pub(super) fn handle(agent_cmd: AgentCommands, json: bool) -> Result<(), CliError> {
    let cwd = std::env::current_dir()
        .map_err(|e| CliError::Other(format!("cannot determine working directory: {e}")))?;
    match agent_cmd {
        AgentCommands::Init {
            server,
            repo,
            prompt_template,
        } => {
            let result = agent::init(
                &cwd,
                server.as_deref(),
                repo.as_deref(),
                prompt_template.as_deref(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                agent::print_init_result(&result);
            }
        }
        AgentCommands::Claim { server, repo } => {
            let outcome = agent::claim(&cwd, server.as_deref(), repo.as_deref())?;
            if json {
                match &outcome {
                    agent::ClaimOutcome::Claimed(state)
                    | agent::ClaimOutcome::AlreadyClaimed(state) => {
                        println!("{}", serde_json::to_string_pretty(state).unwrap());
                    }
                    agent::ClaimOutcome::NoWork => {
                        println!("{{\"status\":\"no_work\"}}");
                        std::process::exit(1);
                    }
                }
            } else {
                agent::print_claim_result(&outcome);
                if matches!(outcome, agent::ClaimOutcome::NoWork) {
                    std::process::exit(1);
                }
            }
        }
        AgentCommands::Download { dir } => {
            let result = agent::download(&cwd, &dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                agent::print_download_result(&result);
            }
        }
        AgentCommands::Issue => {
            if json {
                let raw = agent::fetch_issue_raw(&cwd)?;
                println!("{raw}");
            } else {
                let detail = agent::fetch_issue(&cwd)?;
                agent::print_issue_detail(&detail);
            }
        }
        AgentCommands::Status => {
            let result = agent::status(&cwd);
            match result {
                Ok(r) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&r).unwrap());
                    } else {
                        agent::print_status_result(&r);
                    }
                }
                Err(agent::AgentError::NoState) => {
                    if json {
                        println!("{{\"error\":\"no active agent state\"}}");
                    } else {
                        eprintln!("No active agent state — run `vai agent claim` first.");
                    }
                    std::process::exit(1);
                }
                Err(e) => return Err(e.into()),
            }
        }
        AgentCommands::Reset => {
            let result = agent::reset(&cwd);
            match result {
                Ok(r) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&r).unwrap());
                    } else {
                        agent::print_reset_result(&r);
                    }
                }
                Err(agent::AgentError::NoState) => {
                    if json {
                        println!("{{\"error\":\"no active agent state\"}}");
                    } else {
                        eprintln!("No active agent state — nothing to reset.");
                    }
                    std::process::exit(1);
                }
                Err(e) => return Err(e.into()),
            }
        }
        AgentCommands::Prompt { template } => {
            let result =
                agent::prompt(&cwd, template.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                if let Some(hint) = &result.deprecation_hint {
                    eprintln!("{hint}");
                }
                print!("{}", result.prompt);
            }
        }
        AgentCommands::Submit { dir, close_if_empty } => {
            match agent::submit(&cwd, &dir) {
                Ok(result) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result).unwrap());
                    } else {
                        agent::print_submit_result(&result);
                    }
                }
                Err(agent::AgentError::WorkspaceEmpty) if close_if_empty => {
                    match agent::close_issue_as_resolved(&cwd) {
                        Ok(result) => {
                            if json {
                                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                            } else {
                                agent::print_close_resolved_result(&result);
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(agent::AgentError::WorkspaceEmpty) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "error": "workspace_empty",
                                "message": "No changes to submit — the issue appears already resolved.",
                                "hint": "Re-run with --close-if-empty to close the issue permanently instead of resetting."
                            })
                        );
                    } else {
                        eprintln!("No changes to submit — the issue appears already resolved.");
                        eprintln!(
                            "Hint: re-run with `--close-if-empty` to close it permanently \
                             instead of resetting (which re-opens it)."
                        );
                    }
                    std::process::exit(3);
                }
                Err(e) => return Err(e.into()),
            }
        }
        AgentCommands::Setup { dir } => {
            let result = agent::setup(&dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                if !result.all_passed {
                    std::process::exit(1);
                }
            } else if result.no_setup_configured {
                // Exit 2 signals "no setup configured" to loop.sh so it can
                // decide whether to run an implicit pnpm install fallback.
                std::process::exit(2);
            } else if result.all_passed {
                use colored::Colorize;
                let count = result.commands.len();
                println!(
                    "{} {} setup command{} ran",
                    "✓".green().bold(),
                    count,
                    if count == 1 { "" } else { "s" }
                );
            } else {
                use colored::Colorize;
                let failed: Vec<_> = result.commands.iter().filter(|c| !c.passed).collect();
                eprintln!(
                    "{} {}/{} setup command{} failed",
                    "✗".red().bold(),
                    failed.len(),
                    result.commands.len(),
                    if failed.len() == 1 { "" } else { "s" }
                );
                for cmd in &failed {
                    eprintln!("[setup] {} (exit {})", cmd.command, cmd.exit_code);
                    if !cmd.stderr.is_empty() {
                        eprint!("{}", cmd.stderr);
                    }
                }
                std::process::exit(1);
            }
        }
        AgentCommands::Loop(loop_cmd) => {
            return agent_loop::handle_loop(loop_cmd, json);
        }
        AgentCommands::Verify { dir } => {
            let result = agent::verify(&cwd, &dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                if !result.all_passed {
                    std::process::exit(1);
                }
            } else if result.no_checks_configured {
                eprintln!(
                    "warning: no checks configured — add an [agent] section to vai.toml \
                     (or a legacy [checks] section in .vai/agent.toml) to enable verification"
                );
            } else if result.all_passed {
                use colored::Colorize;
                let count = result.checks.len();
                println!("{} All {} check{} passed", "✓".green().bold(), count, if count == 1 { "" } else { "s" });
            } else {
                use colored::Colorize;
                let failed: Vec<_> = result.checks.iter().filter(|c| !c.passed).collect();
                eprintln!("{} {}/{} check{} failed", "✗".red().bold(), failed.len(), result.checks.len(), if failed.len() == 1 { "" } else { "s" });
                agent::print_verify_errors(&result);
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
