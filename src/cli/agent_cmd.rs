//! Agent command handlers.

use crate::agent;
use super::{CliError, AgentCommands};

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
                print!("{}", result.prompt);
            }
        }
        AgentCommands::Submit { dir } => {
            let result = agent::submit(&cwd, &dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
            } else {
                agent::print_submit_result(&result);
            }
        }
        AgentCommands::Verify { dir } => {
            let result = agent::verify(&cwd, &dir)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                if !result.all_passed {
                    std::process::exit(1);
                }
            } else if result.no_checks_configured {
                eprintln!("warning: no checks configured in .vai/agent.toml — nothing to verify");
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
