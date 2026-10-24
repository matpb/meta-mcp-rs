//! Interactive prompts; non-interactive runs fail with a usage error that names the flag to pass.

use std::io::{BufRead, IsTerminal, Write};

use super::SetupError;
use super::report;
use crate::config::secret::Secret;

/// Asks questions on the terminal, or refuses to when prompts are disabled.
#[derive(Debug, Clone, Copy)]
pub struct Prompter {
    interactive: bool,
    assume_yes: bool,
}

const MAX_EMPTY_SECRETS: usize = 3;

fn input_closed() -> SetupError {
    SetupError::usage("input closed before an answer arrived; pass the value as a flag instead")
}

fn read_answer(prompt: &str) -> Result<String, SetupError> {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    drop(out);
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => Err(input_closed()),
        Ok(_) => Ok(line.trim().to_string()),
    }
}

/// Parses picks like `1,3`, `2-4` or `all` into sorted zero-based indexes.
pub fn parse_selection(input: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut picked = Vec::new();
    for token in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        if token.eq_ignore_ascii_case("all") || token == "*" {
            return Ok((0..count).collect());
        }
        let (from, to) = match token.split_once('-') {
            Some((a, b)) => (a.parse::<usize>(), b.parse::<usize>()),
            None => (token.parse::<usize>(), token.parse::<usize>()),
        };
        let (Ok(from), Ok(to)) = (from, to) else {
            return Err(format!("`{token}` is not a number from the list"));
        };
        if from == 0 || to < from || to > count {
            return Err(format!("`{token}` is outside 1 to {count}"));
        }
        picked.extend(from - 1..to);
    }
    picked.sort_unstable();
    picked.dedup();
    if picked.is_empty() {
        return Err("pick at least one number".to_string());
    }
    Ok(picked)
}

impl Prompter {
    /// Prompts are on only when stdin is a terminal and `--non-interactive` is absent.
    pub fn new(non_interactive: bool, assume_yes: bool) -> Self {
        Self {
            interactive: !non_interactive && std::io::stdin().is_terminal(),
            assume_yes,
        }
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn missing(flag: &str) -> SetupError {
        SetupError::usage(format!(
            "missing value: pass {flag} (prompts are off in non-interactive mode)"
        ))
    }

    pub fn text(
        &self,
        flag: &str,
        label: &str,
        default: Option<&str>,
    ) -> Result<String, SetupError> {
        if !self.interactive {
            return Err(Self::missing(flag));
        }
        let hint = default.map(|d| format!(" [{d}]")).unwrap_or_default();
        loop {
            let answer = read_answer(&format!("{label}{hint}: "))?;
            if !answer.is_empty() {
                return Ok(answer);
            }
            if let Some(d) = default {
                return Ok(d.to_string());
            }
        }
    }

    /// Reads a secret without echo.
    pub fn secret(&self, flag: &str, label: &str) -> Result<Secret, SetupError> {
        if !self.interactive {
            return Err(Self::missing(flag));
        }
        for _ in 0..MAX_EMPTY_SECRETS {
            let value = rpassword::prompt_password(format!("{label} (hidden): ")).map_err(|e| {
                SetupError::fatal(format!("cannot read the secret from the terminal: {e}"))
                    .with_fix(format!("pass {flag} instead of typing it"))
            })?;
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Secret::new(value));
            }
        }
        Err(SetupError::usage(format!(
            "no secret was entered; pass {flag} instead"
        )))
    }

    /// A yes or no that guards an overwrite: `--yes` and non-interactive runs answer `default`.
    pub fn confirm(&self, label: &str, default: bool) -> bool {
        if self.assume_yes {
            return true;
        }
        self.ask_yes_no(label, default)
    }

    /// A yes or no for an optional feature: only a person at the terminal can opt in.
    pub fn offer(&self, label: &str, default: bool) -> bool {
        self.ask_yes_no(label, default)
    }

    fn ask_yes_no(&self, label: &str, default: bool) -> bool {
        if !self.interactive {
            return default;
        }
        let hint = if default { "[Y/n]" } else { "[y/N]" };
        match read_answer(&format!("{label} {hint}: ")) {
            Ok(a) if a.is_empty() => default,
            Ok(a) => matches!(a.to_ascii_lowercase().as_str(), "y" | "yes"),
            Err(_) => default,
        }
    }

    /// Numbered pick of exactly one option; Enter takes the first.
    pub fn choose_one(
        &self,
        flag: &str,
        label: &str,
        options: &[String],
    ) -> Result<usize, SetupError> {
        if !self.interactive {
            return Err(Self::missing(flag));
        }
        report::say(label);
        for (i, option) in options.iter().enumerate() {
            report::say(&format!("  {}. {option}", i + 1));
        }
        loop {
            let answer = read_answer("Your choice [1]: ")?;
            if answer.is_empty() {
                return Ok(0);
            }
            match parse_selection(&answer, options.len()) {
                Ok(picked) if picked.len() == 1 => return Ok(picked[0]),
                Ok(_) => report::line(report::Status::Warn, "pick exactly one number"),
                Err(e) => report::line(report::Status::Warn, &e),
            }
        }
    }

    /// Numbered pick list; Enter selects the only option when `single_is_default` is set.
    pub fn choose(
        &self,
        flag: &str,
        label: &str,
        options: &[String],
        single_is_default: bool,
    ) -> Result<Vec<usize>, SetupError> {
        if !self.interactive {
            return Err(Self::missing(flag));
        }
        report::say(label);
        for (i, option) in options.iter().enumerate() {
            report::say(&format!("  {}. {option}", i + 1));
        }
        loop {
            let answer = read_answer("Pick numbers separated by commas, or `all`: ")?;
            if answer.is_empty() && single_is_default && options.len() == 1 {
                return Ok(vec![0]);
            }
            match parse_selection(&answer, options.len()) {
                Ok(picked) => return Ok(picked),
                Err(e) => report::line(report::Status::Warn, &e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_parse_numbers_ranges_and_all() {
        assert_eq!(parse_selection("1", 3), Ok(vec![0]));
        assert_eq!(parse_selection("3, 1", 3), Ok(vec![0, 2]));
        assert_eq!(parse_selection("2-4", 5), Ok(vec![1, 2, 3]));
        assert_eq!(parse_selection("all", 2), Ok(vec![0, 1]));
        assert_eq!(parse_selection("1,1 1", 2), Ok(vec![0]));
        assert!(parse_selection("", 3).is_err());
        assert!(parse_selection("0", 3).is_err());
        assert!(parse_selection("4", 3).is_err());
        assert!(parse_selection("3-1", 3).is_err());
        assert!(parse_selection("x", 3).is_err());
    }

    #[test]
    fn non_interactive_prompts_are_usage_errors_naming_the_flag() {
        let p = Prompter {
            interactive: false,
            assume_yes: false,
        };
        let err = p.text("--app-id", "App ID", None).expect_err("usage");
        assert!(
            matches!(&err, SetupError::Usage(m) if m.contains("--app-id")),
            "{err:?}"
        );
        assert!(p.secret("--app-secret-env", "Secret").is_err());
        assert!(p.choose("--page-id", "Pages", &["a".into()], true).is_err());
        assert!(!p.offer("Extra?", false));
        assert!(!p.confirm("Overwrite?", false));
        let yes = Prompter {
            interactive: false,
            assume_yes: true,
        };
        assert!(yes.confirm("Overwrite?", false));
        assert!(!yes.offer("Extra?", false));
    }
}
