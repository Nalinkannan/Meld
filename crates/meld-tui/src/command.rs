//! Meld command bar parser.
//!
//! Commands are typed into the persistent command bar (focused with `:` from
//! `Normal` mode). The parser produces a `MeldCommand` enum which is
//! dispatched to real engine/store calls. Arbitrary text is never run as SQL
//! and there is no OS shell.

/// A parsed Meld command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeldCommand {
    Help,
    Connect { profile: String },
    Disconnect,
    Snapshot { message: String },
    History,
    Diff { a: String, b: String },
    Branches,
    Merge { source: String },
    Restore { commit: String },
    Verify { commit: Option<String> },
    Drift,
    Query,
}

/// Parse a command line (without the leading `:`).
pub fn parse_command(input: &str) -> Result<MeldCommand, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty command".into());
    }
    let mut parts = input.split_whitespace();
    let cmd = parts.next().unwrap();
    let rest: Vec<&str> = parts.collect();

    match cmd {
        "help" | "h" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to help: {}", rest[0]));
            }
            Ok(MeldCommand::Help)
        }
        "connect" | "c" => {
            let profile = rest
                .first()
                .ok_or_else(|| "connect requires a profile name".to_string())?;
            Ok(MeldCommand::Connect {
                profile: profile.to_string(),
            })
        }
        "disconnect" | "dc" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to disconnect: {}", rest[0]));
            }
            Ok(MeldCommand::Disconnect)
        }
        "snapshot" | "s" => {
            let message = rest
                .first()
                .ok_or_else(|| "snapshot requires a message".to_string())?;
            Ok(MeldCommand::Snapshot {
                message: message.to_string(),
            })
        }
        "history" | "hi" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to history: {}", rest[0]));
            }
            Ok(MeldCommand::History)
        }
        "diff" | "d" => {
            let a = rest
                .first()
                .ok_or_else(|| "diff requires two operands: diff <a> <b>".to_string())?
                .to_string();
            let b = rest
                .get(1)
                .ok_or_else(|| "diff requires two operands: diff <a> <b>".to_string())?
                .to_string();
            Ok(MeldCommand::Diff { a, b })
        }
        "branches" | "b" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to branches: {}", rest[0]));
            }
            Ok(MeldCommand::Branches)
        }
        "merge" | "m" => {
            let source = rest
                .first()
                .ok_or_else(|| "merge requires a source branch: merge <source>".to_string())?;
            Ok(MeldCommand::Merge {
                source: source.to_string(),
            })
        }
        "restore" | "r" => {
            let commit = rest
                .first()
                .ok_or_else(|| "restore requires a commit: restore <commit>".to_string())?;
            Ok(MeldCommand::Restore {
                commit: commit.to_string(),
            })
        }
        "verify" | "v" => {
            let commit = if rest.is_empty() {
                None
            } else {
                Some(rest[0].to_string())
            };
            Ok(MeldCommand::Verify { commit })
        }
        "drift" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to drift: {}", rest[0]));
            }
            Ok(MeldCommand::Drift)
        }
        "query" => {
            if !rest.is_empty() {
                return Err(format!("unknown argument to query: {}", rest[0]));
            }
            Ok(MeldCommand::Query)
        }
        _ => {
            let suggestions = suggest(cmd);
            if suggestions.is_empty() {
                Err(format!("unknown command: {cmd}"))
            } else {
                Err(format!(
                    "unknown command: {cmd}. Did you mean: {}",
                    suggestions.join(", ")
                ))
            }
        }
    }
}

/// Tab-complete a partial command line. Returns the completed line or None.
pub fn complete_command(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let mut parts = input.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("");

    let commands = [
        "branches",
        "connect",
        "disconnect",
        "diff",
        "drift",
        "help",
        "history",
        "merge",
        "query",
        "restore",
        "snapshot",
        "verify",
    ];
    let matches: Vec<&str> = commands
        .iter()
        .filter(|c| c.starts_with(head))
        .copied()
        .collect();
    match matches.len() {
        1 => {
            let completed = if tail.is_empty() {
                matches[0].to_string() + " "
            } else {
                matches[0].to_string() + " " + tail
            };
            Some(completed)
        }
        _ => None,
    }
}

/// Suggest similar command names for an unknown command.
fn suggest(cmd: &str) -> Vec<String> {
    let commands = [
        "branches",
        "connect",
        "disconnect",
        "diff",
        "drift",
        "help",
        "history",
        "merge",
        "query",
        "restore",
        "snapshot",
        "verify",
    ];
    let mut scored: Vec<(usize, &str)> = commands
        .iter()
        .copied()
        .filter(|c| {
            // Simple suggestion: shares a prefix or is a common prefix of cmd.
            c.starts_with(cmd) || cmd.starts_with(c) || levenshtein(cmd, c) <= 2
        })
        .map(|c| (levenshtein(cmd, c), c))
        .collect();
    scored.sort_by_key(|(d, _)| *d);
    scored
        .into_iter()
        .take(3)
        .map(|(_, c)| c.to_string())
        .collect()
}

/// Levenshtein distance (bounded).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = std::cmp::min(
                std::cmp::min(cur[j] + 1, prev[j + 1] + 1),
                prev[j] + if ca == cb { 0 } else { 1 },
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_commands() {
        assert!(matches!(parse_command("help"), Ok(MeldCommand::Help)));
        assert!(matches!(
            parse_command("connect main"),
            Ok(MeldCommand::Connect { profile }) if profile == "main"
        ));
        assert!(matches!(
            parse_command("disconnect"),
            Ok(MeldCommand::Disconnect)
        ));
        assert!(matches!(
            parse_command("snapshot initial load"),
            Ok(MeldCommand::Snapshot { message }) if message == "initial load"
        ));
        assert!(matches!(
            parse_command("diff a b"),
            Ok(MeldCommand::Diff { a, b }) if a == "a" && b == "b"
        ));
        assert!(matches!(
            parse_command("diff live main"),
            Ok(MeldCommand::Diff { .. })
        ));
        assert!(matches!(
            parse_command("verify"),
            Ok(MeldCommand::Verify { commit: None })
        ));
        assert!(matches!(
            parse_command("verify abc123"),
            Ok(MeldCommand::Verify { commit }) if commit == Some("abc123".to_string())
        ));
    }

    #[test]
    fn parse_unknown_command_suggests() {
        let err = parse_command("connet main").unwrap_err();
        assert!(err.contains("connect"));
    }

    #[test]
    fn complete_command_unambiguous() {
        assert_eq!(complete_command("con"), Some("connect ".to_string()));
        assert_eq!(complete_command("h"), Some("history ".to_string()));
    }
}
